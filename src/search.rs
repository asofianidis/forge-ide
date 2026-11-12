//! Project-wide file listing and text search, using the crates behind ripgrep.
//!
//! Both walk the workspace with `ignore`, so `.gitignore`, `.ignore` and hidden
//! files are skipped the same way ripgrep skips them. Work runs on a background
//! thread and results stream back over a channel.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;

/// Stop after this many matching lines.
pub const MAX_RESULTS: usize = 10_000;
/// Stop listing files for quick open after this many.
pub const MAX_FILES: usize = 100_000;
/// Longest line shown in results; longer lines are cut around the match.
const MAX_LINE_CHARS: usize = 240;

fn walker(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        // Respect .gitignore even outside a git repository.
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
}

/// A flag the UI sets to abandon a running job.
#[derive(Clone, Default)]
struct Cancel(Arc<AtomicBool>);

impl Cancel {
    fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

// ---- file index (quick open) -----------------------------------------------

/// Files in the workspace, relative to its root, gathered in the background.
#[derive(Default)]
pub struct FileIndex {
    pub root: Option<PathBuf>,
    pub files: Vec<String>,
    rx: Option<Receiver<Vec<String>>>,
    cancel: Cancel,
}

impl FileIndex {
    /// Starts (re)listing files under `root`. Old results stay visible until
    /// the new list arrives.
    pub fn refresh(&mut self, root: &Path, ctx: &egui::Context) {
        self.cancel.cancel();
        if self.root.as_deref() != Some(root) {
            self.files.clear();
        }
        self.root = Some(root.to_path_buf());
        let cancel = Cancel::default();
        self.cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let root = root.to_path_buf();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut files = Vec::new();
            for entry in walker(&root).filter_map(Result::ok) {
                if cancel.is_cancelled() || files.len() >= MAX_FILES {
                    break;
                }
                if entry.file_type().is_some_and(|t| t.is_file())
                    && let Ok(rel) = entry.path().strip_prefix(&root)
                {
                    files.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
            files.sort_unstable();
            if !cancel.is_cancelled() {
                let _ = tx.send(files);
                ctx.request_repaint();
            }
        });
    }

    pub fn is_loading(&self) -> bool {
        self.rx.is_some()
    }

    /// Picks up a finished listing. Returns true if the file list changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(files) => {
                self.files = files;
                self.rx = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                false
            }
        }
    }
}

// ---- text search -------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub pattern: String,
    pub match_case: bool,
    pub regex: bool,
    pub whole_word: bool,
}

/// A matching line.
#[derive(Debug, Clone)]
pub struct LineMatch {
    /// 1-based line number.
    pub line: usize,
    /// Byte range of the first match within the full line, used to place the cursor.
    pub first: Range<usize>,
    /// The line as displayed (trimmed and possibly shortened).
    pub preview: String,
    /// Byte ranges of matches within `preview`.
    pub highlights: Vec<Range<usize>>,
}

#[derive(Debug, Clone)]
pub struct FileMatches {
    pub path: PathBuf,
    /// Path relative to the search root, for display.
    pub display: String,
    pub lines: Vec<LineMatch>,
}

enum Message {
    File(FileMatches),
    Done { truncated: bool },
}

#[derive(Default)]
pub struct ProjectSearch {
    pub query: SearchQuery,
    pub results: Vec<FileMatches>,
    pub error: Option<String>,
    /// The result limit was hit, so `results` is incomplete.
    pub truncated: bool,
    rx: Option<Receiver<Message>>,
    cancel: Cancel,
}

impl ProjectSearch {
    pub fn is_running(&self) -> bool {
        self.rx.is_some()
    }

    pub fn match_count(&self) -> usize {
        self.results.iter().map(|f| f.lines.len()).sum()
    }

    pub fn clear(&mut self) {
        self.cancel.cancel();
        self.rx = None;
        self.results.clear();
        self.error = None;
        self.truncated = false;
    }

    /// Starts searching `root` for `self.query`, replacing previous results.
    pub fn start(&mut self, root: &Path, ctx: &egui::Context) {
        self.clear();
        if self.query.pattern.is_empty() {
            return;
        }
        let matcher = match build_matcher(&self.query) {
            Ok(m) => m,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let cancel = Cancel::default();
        self.cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let root = root.to_path_buf();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let truncated = search_tree(&root, &matcher, &cancel, |file| {
                let ok = tx.send(Message::File(file)).is_ok();
                ctx.request_repaint();
                ok
            });
            let _ = tx.send(Message::Done { truncated });
            ctx.request_repaint();
        });
    }

    /// Collects results that arrived since the last frame.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok(Message::File(f)) => self.results.push(f),
                Ok(Message::Done { truncated }) => {
                    self.truncated = truncated;
                    self.rx = None;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.rx = None;
                    break;
                }
            }
        }
    }
}

fn build_matcher(query: &SearchQuery) -> Result<RegexMatcher, String> {
    RegexMatcherBuilder::new()
        .case_insensitive(!query.match_case)
        .fixed_strings(!query.regex)
        .word(query.whole_word)
        .build(&query.pattern)
        .map_err(|e| e.to_string())
}

/// Searches every non-ignored file under `root`, calling `emit` for each file
/// with matches (stopping early if it returns false). Returns whether the
/// result limit was reached.
fn search_tree(
    root: &Path,
    matcher: &RegexMatcher,
    cancel: &Cancel,
    mut emit: impl FnMut(FileMatches) -> bool,
) -> bool {
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build();
    let mut total = 0;
    for entry in walker(root).filter_map(Result::ok) {
        if cancel.is_cancelled() {
            return false;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let mut lines = Vec::new();
        let result = searcher.search_path(
            matcher,
            path,
            Lossy(|line_number, line| {
                if let Some(m) = line_match(matcher, line_number as usize, line) {
                    lines.push(m);
                    total += 1;
                }
                Ok(total < MAX_RESULTS && !cancel.is_cancelled())
            }),
        );
        if result.is_err() || lines.is_empty() {
            continue;
        }
        let display = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let file = FileMatches {
            path: path.to_path_buf(),
            display,
            lines,
        };
        if !emit(file) {
            return false;
        }
        if total >= MAX_RESULTS {
            return true;
        }
    }
    false
}

fn line_match(matcher: &RegexMatcher, line: usize, text: &str) -> Option<LineMatch> {
    let text = text.trim_end_matches(['\n', '\r']);
    let mut ranges = Vec::new();
    let _ = matcher.find_iter(text.as_bytes(), |m| {
        if !m.is_empty() {
            ranges.push(m.start()..m.end());
        }
        ranges.len() < 100
    });
    let first = ranges.first()?.clone();
    let (preview, offset) = preview_window(text, first.start);
    let highlights = ranges
        .into_iter()
        .filter(|r| r.start >= offset && r.end <= offset + preview.len())
        .map(|r| r.start - offset..r.end - offset)
        .collect();
    Some(LineMatch {
        line,
        first,
        preview: preview.to_owned(),
        highlights,
    })
}

/// A displayable slice of `line`: leading whitespace trimmed and, for long
/// lines, a window that keeps `focus` (a byte offset) visible. Returns the
/// slice and its byte offset in `line`.
fn preview_window(line: &str, focus: usize) -> (&str, usize) {
    let mut start = line.len() - line.trim_start().len();
    if focus < start {
        start = focus;
    }
    let rest = &line[start..];
    if rest.chars().count() <= MAX_LINE_CHARS {
        return (rest, start);
    }
    // Keep some context before the match.
    let before = line[start..focus].chars().count();
    if before > 40 {
        let skip = before - 40;
        start += line[start..].char_indices().nth(skip).map_or(0, |(b, _)| b);
    }
    let rest = &line[start..];
    let end = rest
        .char_indices()
        .nth(MAX_LINE_CHARS)
        .map_or(rest.len(), |(b, _)| b);
    (&rest[..end], start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("forge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        std::fs::write(root.join("src/a.rs"), "fn needle() {}\n  let Needle = 1;\n").unwrap();
        std::fs::write(root.join("build/out.rs"), "needle\n").unwrap();
        std::fs::write(root.join("bin.dat"), b"needle\0\x01").unwrap();
        root
    }

    #[test]
    fn search_respects_gitignore_and_options() {
        let root = temp_project("search");
        let matcher = build_matcher(&SearchQuery {
            pattern: "needle".into(),
            ..Default::default()
        })
        .unwrap();
        let mut files = Vec::new();
        search_tree(&root, &matcher, &Cancel::default(), |f| {
            files.push(f);
            true
        });
        let case_sensitive = build_matcher(&SearchQuery {
            pattern: "Needle".into(),
            match_case: true,
            ..Default::default()
        })
        .unwrap();
        let mut exact = Vec::new();
        search_tree(&root, &case_sensitive, &Cancel::default(), |f| {
            exact.push(f);
            true
        });
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].display, "src/a.rs");
        assert_eq!(files[0].lines.len(), 2);
        let second = &files[0].lines[1];
        assert_eq!(second.line, 2);
        assert_eq!(second.first, 6..12);
        assert_eq!(second.preview, "let Needle = 1;");
        assert_eq!(second.highlights, vec![4..10]);
        assert_eq!(exact[0].lines.len(), 1);
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let q = SearchQuery {
            pattern: "(".into(),
            regex: true,
            ..Default::default()
        };
        assert!(build_matcher(&q).is_err());
    }

    #[test]
    fn long_lines_keep_match_in_preview() {
        let line = format!("{}needle{}", "x".repeat(500), "y".repeat(500));
        let (preview, offset) = preview_window(&line, 500);
        assert!(preview.len() <= MAX_LINE_CHARS);
        assert!(preview.contains("needle"));
        assert_eq!(&line[offset..offset + preview.len()], preview);
    }
}

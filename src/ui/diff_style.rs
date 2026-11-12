//! Styling for diff lines: syntax highlighting (with the editor's theme)
//! and word-level emphasis of what changed between paired lines.

use std::cell::RefCell;
use std::ops::Range;
use std::time::{Duration, Instant};

use egui::Color32;
use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

use crate::git::diff::{FileDiff, LineKind};

/// Style for one diff line: colored byte ranges of its text, plus the
/// ranges that changed relative to its paired line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineStyle {
    pub spans: Vec<(Range<usize>, Color32)>,
    pub emph: Vec<Range<usize>>,
}

/// Lines longer than this skip highlighting and word diffs (minified files).
const MAX_STYLED_LINE: usize = 1000;

struct Syntax {
    syntaxes: SyntaxSet,
    theme: Option<Theme>,
    /// Bumped when the theme changes, to invalidate cached styles.
    generation: u64,
}

thread_local! {
    static SYNTAX: RefCell<Option<Syntax>> = const { RefCell::new(None) };
}

fn with_syntax<R>(f: impl FnOnce(&mut Syntax) -> R) -> R {
    SYNTAX.with(|s| {
        let mut s = s.borrow_mut();
        let syntax = s.get_or_insert_with(|| Syntax {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme: None,
            generation: 0,
        });
        f(syntax)
    })
}

/// Uses `theme` for diff highlighting (call when the editor theme changes).
pub fn set_theme(theme: Theme) {
    with_syntax(|s| {
        s.theme = Some(theme);
        s.generation += 1;
    });
}

pub fn generation() -> u64 {
    with_syntax(|s| s.generation)
}

/// Styles every line of every hunk in `file`, indexed `[hunk][line]`.
pub fn style_file(file: &FileDiff) -> Vec<Vec<LineStyle>> {
    let ext = file
        .path()
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map_or("", |(_, e)| e)
        .to_owned();
    let mut out: Vec<Vec<LineStyle>> = file
        .hunks
        .iter()
        .map(|h| vec![LineStyle::default(); h.lines.len()])
        .collect();

    // Highlight each side as one continuous text so multi-line constructs
    // (strings, comments) keep their state across lines.
    for side in [LineKind::Removed, LineKind::Added] {
        let mut positions = Vec::new();
        let mut lines = Vec::new();
        for (h, hunk) in file.hunks.iter().enumerate() {
            for (i, line) in hunk.lines.iter().enumerate() {
                let on_side = line.kind == side || line.kind == LineKind::Context;
                if on_side {
                    positions.push((h, i, line.kind == LineKind::Context));
                    lines.push(line.text.as_str());
                }
            }
        }
        let spans = highlight(&lines, &ext);
        for ((h, i, context), spans) in positions.into_iter().zip(spans) {
            // Context lines appear on both sides; keep the new side's.
            if !context || side == LineKind::Added {
                out[h][i].spans = spans;
            }
        }
    }

    for (h, hunk) in file.hunks.iter().enumerate() {
        for (removed, added) in change_runs(&hunk.lines.iter().map(|l| l.kind).collect::<Vec<_>>())
        {
            for (&r, &a) in removed.iter().zip(&added) {
                if let Some((old, new)) = word_ranges(&hunk.lines[r].text, &hunk.lines[a].text) {
                    out[h][r].emph = old;
                    out[h][a].emph = new;
                }
            }
        }
    }
    out
}

/// Runs of removed lines and the added lines that replace them, as index
/// lists. Used for pairing lines in word diffs and the split view.
pub fn change_runs(kinds: &[LineKind]) -> Vec<(Vec<usize>, Vec<usize>)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < kinds.len() {
        if kinds[i] == LineKind::Context {
            i += 1;
            continue;
        }
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        while i < kinds.len() && kinds[i] != LineKind::Context {
            match kinds[i] {
                LineKind::Removed => removed.push(i),
                _ => added.push(i),
            }
            i += 1;
        }
        runs.push((removed, added));
    }
    runs
}

fn highlight(lines: &[&str], ext: &str) -> Vec<Vec<(Range<usize>, Color32)>> {
    with_syntax(|s| {
        let Some(theme) = &s.theme else {
            return vec![Vec::new(); lines.len()];
        };
        let syntax = s
            .syntaxes
            .find_syntax_by_extension(ext)
            .unwrap_or_else(|| s.syntaxes.find_syntax_plain_text());
        let mut hl = HighlightLines::new(syntax, theme);
        lines
            .iter()
            .map(|line| {
                if line.len() > MAX_STYLED_LINE {
                    return Vec::new();
                }
                let with_newline = format!("{line}\n");
                let Ok(ranges) = hl.highlight_line(&with_newline, &s.syntaxes) else {
                    return Vec::new();
                };
                let mut pos = 0;
                let mut spans = Vec::new();
                for (style, piece) in ranges {
                    let end = (pos + piece.len()).min(line.len());
                    if pos < end {
                        let c = style.foreground;
                        spans.push((pos..end, Color32::from_rgb(c.r, c.g, c.b)));
                    }
                    pos += piece.len();
                }
                spans
            })
            .collect()
    })
}

/// Changed byte ranges in an old line and its replacement.
pub type WordRanges = (Vec<Range<usize>>, Vec<Range<usize>>);

/// Splits a line into code tokens: identifier/number runs, whitespace
/// runs, and single punctuation characters. Returns byte ranges.
fn tokenize(line: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut tokens: Vec<Range<usize>> = Vec::new();
    let mut prev: Option<Class> = None;
    for (i, c) in line.char_indices() {
        let k = class(c);
        let joins = matches!((&prev, &k), (Some(p), k) if p == k && *k != Class::Other);
        match tokens.last_mut() {
            Some(last) if joins => last.end = i + c.len_utf8(),
            _ => tokens.push(i..i + c.len_utf8()),
        }
        prev = Some(k);
    }
    tokens
}

/// Byte ranges that differ between `old` and `new`, token by token. `None`
/// when the lines are too different for emphasis to help.
pub fn word_ranges(old: &str, new: &str) -> Option<WordRanges> {
    if old.len() > MAX_STYLED_LINE || new.len() > MAX_STYLED_LINE {
        return None;
    }
    let old_t = tokenize(old);
    let new_t = tokenize(new);
    let old_s: Vec<&str> = old_t.iter().map(|r| &old[r.clone()]).collect();
    let new_s: Vec<&str> = new_t.iter().map(|r| &new[r.clone()]).collect();
    let deadline = Instant::now() + Duration::from_millis(5);
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &old_s, &new_s, Some(deadline));
    let (mut old_r, mut new_r): WordRanges = (Vec::new(), Vec::new());
    let push = |v: &mut Vec<Range<usize>>, r: Range<usize>| match v.last_mut() {
        Some(last) if last.end == r.start => last.end = r.end,
        _ => v.push(r),
    };
    let span =
        |tokens: &[Range<usize>], at: usize, len: usize| tokens[at].start..tokens[at + len - 1].end;
    for op in ops {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete {
                old_index, old_len, ..
            } => push(&mut old_r, span(&old_t, old_index, old_len)),
            DiffOp::Insert {
                new_index, new_len, ..
            } => push(&mut new_r, span(&new_t, new_index, new_len)),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                push(&mut old_r, span(&old_t, old_index, old_len));
                push(&mut new_r, span(&new_t, new_index, new_len));
            }
        }
    }
    let changed: usize = old_r.iter().chain(&new_r).map(|r| r.len()).sum();
    let total = old.len() + new.len();
    // Mostly rewritten lines read better without emphasis.
    if total == 0 || changed * 10 > total * 7 {
        return None;
    }
    Some((old_r, new_r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff;

    #[test]
    fn word_ranges_mark_changed_words() {
        let (old, new) = word_ranges("let x = foo(1);", "let x = bar(1);").unwrap();
        assert_eq!(&"let x = foo(1);"[old[0].clone()], "foo");
        assert_eq!(&"let x = bar(1);"[new[0].clone()], "bar");
        assert!(word_ranges("completely different", "nothing alike here").is_none());
        let (old, new) = word_ranges("call(a, b)", "call(a, b, c)").unwrap();
        assert!(old.is_empty());
        assert_eq!(&"call(a, b, c)"[new[0].clone()], ", c");
        assert_eq!(tokenize("a_b+= 12"), vec![0..3, 3..4, 4..5, 5..6, 6..8]);
    }

    #[test]
    fn runs_pair_removed_with_added() {
        use LineKind::*;
        let runs = change_runs(&[Context, Removed, Removed, Added, Context, Added]);
        assert_eq!(runs, vec![(vec![1, 2], vec![3]), (vec![], vec![5])]);
    }

    #[test]
    fn styles_files_with_syntax_and_emphasis() {
        set_theme(crate::theme::Theme::blossom().syntect_theme());
        let files = diff::parse(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n fn main() {}\n-let x = 1;\n+let x = 2;\n",
        );
        let styles = style_file(&files[0]);
        assert_eq!(styles[0].len(), 3);
        assert!(!styles[0][0].spans.is_empty(), "context line highlighted");
        assert!(!styles[0][1].emph.is_empty() && !styles[0][2].emph.is_empty());
    }
}

//! Syntax highlighting via `syntect`, computed lazily per line.
//!
//! Each document keeps a [`HighlightCache`]: the parser state at the start of
//! every line highlighted so far and the styled spans of that line. The editor
//! asks only for the lines it shows, so opening a large file highlights its
//! first screen, not the whole file. Highlighting runs top-down (each line's
//! state depends on the one before), within a time budget per frame.
//!
//! The cache follows the buffer's change log: after an edit only the changed
//! lines are re-parsed, plus any lines below them whose start state changed as
//! a result (e.g. after opening a block comment). As soon as re-parsing
//! reaches a line in the same state as before, the cached lines below are
//! reused as they are.

use std::time::{Duration, Instant};

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter as ThemeHighlighter, Style, Theme,
};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};

use super::Buffer;

use super::decorate::{StyleOverride, semantic_scopes};
use crate::lsp::features::token_mods;

/// Parser and highlighter state at the start of a line.
#[derive(Clone, PartialEq, Eq)]
struct LineState {
    parse: ParseState,
    highlight: HighlightState,
}

/// One highlighted line.
struct Line {
    /// State at the start of the line.
    start: LineState,
    /// Styled byte lengths covering the line (with its line break), or
    /// `None` if parsing failed.
    spans: Option<Vec<(Style, usize)>>,
}

/// Highlighting of one document, kept in step with its buffer.
#[derive(Default)]
pub struct HighlightCache {
    /// Language and theme generation the cache was built for.
    key: Option<(String, u64)>,
    /// Buffer version the line entries match.
    version: u64,
    /// One entry per buffer line; `None` where never highlighted or edited.
    lines: Vec<Option<Line>>,
    /// Entries `[0, valid)` are correct.
    valid: usize,
    /// State at the start of line `valid`.
    next: Option<LineState>,
    /// Entries `[valid, window_end)` are an unbroken run highlighted before
    /// an edit above them: if parsing reaches the start of the run in the
    /// same state as before, the whole run is still correct. `window_state`
    /// is the state at `window_end`.
    window_end: usize,
    window_state: Option<LineState>,
}

impl HighlightCache {
    fn reset(&mut self, key: (String, u64), version: u64, initial: LineState) {
        *self = Self {
            key: Some(key),
            version,
            lines: Vec::new(),
            valid: 0,
            next: Some(initial),
            window_end: 0,
            window_state: None,
        };
    }

    /// Lines highlighted so far (for tests and diagnostics).
    #[cfg(test)]
    fn highlighted(&self) -> usize {
        self.valid
    }

    /// Updates the line entries for an edit: old lines `line..=line + removed`
    /// became new lines `line..=line + inserted`.
    fn apply_line_change(&mut self, line: usize, removed: usize, inserted: usize) {
        if line < self.valid {
            // Lines below the edit that were correct form the new window.
            if self.valid > line + removed + 1 {
                self.window_end = self.valid - removed + inserted;
                self.window_state = self.next.take();
            } else {
                self.window_end = 0;
                self.window_state = None;
            }
            self.next = self.lines[line].as_ref().map(|l| l.start.clone());
            self.valid = line;
        } else if line < self.window_end {
            // An edit inside the window cuts it short at the edited line.
            match self.lines.get(line).and_then(Option::as_ref) {
                Some(l) => {
                    self.window_state = Some(l.start.clone());
                    self.window_end = line;
                }
                None => {
                    self.window_end = 0;
                    self.window_state = None;
                }
            }
        }
        let n = self.lines.len();
        let a = line.min(n);
        let b = (line + removed + 1).min(n);
        if a < n {
            self.lines
                .splice(a..b, std::iter::repeat_with(|| None).take(inserted + 1));
        }
    }

    /// Styled spans of `line` (byte lengths covering the line and its line
    /// break), if it has been highlighted.
    pub fn spans(&self, line: usize) -> Option<&[(Style, usize)]> {
        if line >= self.valid {
            return None;
        }
        self.lines.get(line)?.as_ref()?.spans.as_deref()
    }
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
    /// Bumped on every theme change, invalidating all caches.
    theme_generation: u64,
    /// Theme style of each semantic token type and modifiers seen so far.
    semantic: HashMap<(Arc<str>, u8), Option<StyleOverride>>,
}

impl Highlighter {
    pub fn new(theme: Theme) -> Self {
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme,
            theme_generation: 0,
            semantic: HashMap::new(),
        }
    }

    /// Switches the color theme, invalidating cached highlighting.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.theme_generation += 1;
        self.semantic.clear();
    }

    /// Changes whenever the theme does.
    pub fn generation(&self) -> u64 {
        self.theme_generation
    }

    /// How the theme styles a semantic token, or `None` to leave the syntax
    /// highlighting as it is (no matching theme rule, or a token kind syntax
    /// highlighting handles itself).
    pub fn semantic_style(&mut self, kind: &Arc<str>, modifiers: u8) -> Option<StyleOverride> {
        let key = (Arc::clone(kind), modifiers);
        if let Some(style) = self.semantic.get(&key) {
            return *style;
        }
        let style = self.lookup_semantic(kind, modifiers);
        self.semantic.insert(key, style);
        style
    }

    fn lookup_semantic(&self, kind: &str, modifiers: u8) -> Option<StyleOverride> {
        let scopes = semantic_scopes(
            kind,
            modifiers & token_mods::READONLY != 0,
            modifiers & token_mods::DEFAULT_LIBRARY != 0,
        );
        let theme = ThemeHighlighter::new(&self.theme);
        let found = scopes.iter().find_map(|s| {
            let style = theme.style_mod_for_stack(&[Scope::new(s).ok()?]);
            let fg = style.foreground?;
            Some((Color32::from_rgb(fg.r, fg.g, fg.b), style.font_style))
        });
        let deprecated = modifiers & token_mods::DEPRECATED != 0;
        if found.is_none() && !deprecated {
            return None;
        }
        Some(StyleOverride {
            color: found.map(|(c, _)| c),
            italics: found
                .and_then(|(_, f)| f)
                .map(|f| f.contains(FontStyle::ITALIC)),
            strikethrough: deprecated,
        })
    }

    /// Brings `cache` up to date with `buffer` and highlights lines up to
    /// (not including) `until`, spending at most `budget`. Returns whether
    /// those lines are all highlighted; if not, call again next frame.
    pub fn highlight(
        &self,
        cache: &mut HighlightCache,
        buffer: &Buffer,
        extension: &str,
        until: usize,
        budget: Duration,
    ) -> bool {
        self.sync(cache, buffer, extension);
        let deadline = Instant::now() + budget;
        let theme = ThemeHighlighter::new(&self.theme);
        let rope = buffer.rope();
        let total = rope.len_lines();
        let until = until.min(total);
        let mut line_text = String::new();
        let mut parsed = 0;
        while cache.valid < until {
            let i = cache.valid;
            if i < cache.window_end
                && let (Some(Some(entry)), Some(next)) = (cache.lines.get(i), &cache.next)
                && entry.start == *next
            {
                // Back in step with the lines highlighted before the edit.
                cache.valid = cache.window_end;
                cache.next = cache.window_state.take();
                cache.window_end = 0;
                continue;
            }
            let Some(start) = cache.next.take() else {
                // Shouldn't happen; start over from the top.
                cache.valid = 0;
                cache.window_end = 0;
                cache.window_state = None;
                cache.next = Some(self.initial_state(extension));
                continue;
            };
            line_text.clear();
            for chunk in rope.line(i).chunks() {
                line_text.push_str(chunk);
            }
            let mut state = start.clone();
            let spans = self.highlight_line(&line_text, &mut state, &theme);
            cache.lines[i] = Some(Line { start, spans });
            cache.next = Some(state);
            cache.valid += 1;
            if cache.valid >= cache.window_end {
                cache.window_end = 0;
                cache.window_state = None;
            }
            parsed += 1;
            if parsed % 32 == 0 && Instant::now() > deadline {
                break;
            }
        }
        cache.valid >= until
    }

    /// Follows the buffer's edits, or starts over if they can't be followed.
    fn sync(&self, cache: &mut HighlightCache, buffer: &Buffer, extension: &str) {
        let key = (extension.to_owned(), self.theme_generation);
        if cache.key.as_ref() != Some(&key) {
            cache.reset(key, buffer.version(), self.initial_state(extension));
        } else if cache.version != buffer.version() {
            match buffer.changes_since(cache.version) {
                Some(changes) => {
                    for c in changes {
                        cache.apply_line_change(c.line, c.removed_lines, c.inserted_lines);
                    }
                    cache.version = buffer.version();
                }
                None => cache.reset(key, buffer.version(), self.initial_state(extension)),
            }
        }
        let total = buffer.line_count();
        if cache.lines.len() != total {
            cache.lines.resize_with(total, || None);
            cache.valid = cache.valid.min(total);
            cache.window_end = cache.window_end.min(total);
        }
    }

    fn initial_state(&self, extension: &str) -> LineState {
        let syntax = self
            .syntaxes
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
        LineState {
            parse: ParseState::new(syntax),
            highlight: HighlightState::new(&ThemeHighlighter::new(&self.theme), ScopeStack::new()),
        }
    }

    /// Highlights one line, advancing `state` to the start of the next.
    fn highlight_line(
        &self,
        line: &str,
        state: &mut LineState,
        theme: &ThemeHighlighter<'_>,
    ) -> Option<Vec<(Style, usize)>> {
        let ops = state.parse.parse_line(line, &self.syntaxes).ok()?;
        let spans = HighlightIterator::new(&mut state.highlight, &ops, line, theme)
            .map(|(style, piece)| (style, piece.len()))
            .collect();
        Some(spans)
    }

    /// Appends `text` — the chars `byte_range` of a line whose spans are
    /// `spans` — to `job`, styled. Without spans (not highlighted yet) the
    /// text gets the theme's foreground color.
    pub fn append(
        &self,
        job: &mut LayoutJob,
        text: &str,
        byte_offset: usize,
        spans: Option<&[(Style, usize)]>,
        font: &FontId,
    ) {
        let Some(spans) = spans else {
            job.append(
                text,
                0.0,
                TextFormat::simple(font.clone(), self.foreground()),
            );
            return;
        };
        let end = byte_offset + text.len();
        let mut pos = 0;
        for (style, len) in spans {
            let (a, b) = (pos, pos + len);
            pos = b;
            if b <= byte_offset {
                continue;
            }
            if a >= end {
                break;
            }
            let piece = &text[a.max(byte_offset) - byte_offset..b.min(end) - byte_offset];
            job.append(piece, 0.0, format(style, font));
        }
        if pos < end {
            // Spans cover the line as it was parsed; anything else is plain.
            let from = pos.max(byte_offset) - byte_offset;
            job.append(
                &text[from..],
                0.0,
                TextFormat::simple(font.clone(), self.foreground()),
            );
        }
    }

    pub fn foreground(&self) -> Color32 {
        self.theme
            .settings
            .foreground
            .map(|c| Color32::from_rgb(c.r, c.g, c.b))
            .unwrap_or(Color32::LIGHT_GRAY)
    }

    pub fn background(&self) -> Color32 {
        self.theme
            .settings
            .background
            .map(|c| Color32::from_rgb(c.r, c.g, c.b))
            .unwrap_or(Color32::from_gray(30))
    }
}

fn format(style: &Style, font: &FontId) -> TextFormat {
    let fg = style.foreground;
    let mut format = TextFormat::simple(font.clone(), Color32::from_rgb(fg.r, fg.g, fg.b));
    format.italics = style.font_style.contains(FontStyle::ITALIC);
    if style.font_style.contains(FontStyle::UNDERLINE) {
        format.underline = Stroke::new(1.0, format.color);
    }
    format
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditKind;

    fn highlighter() -> Highlighter {
        let themes = syntect::highlighting::ThemeSet::load_defaults();
        Highlighter::new(themes.themes["base16-ocean.dark"].clone())
    }

    const ALL: Duration = Duration::from_secs(60);

    /// Text and color of every highlighted piece, line by line.
    fn styled(
        h: &Highlighter,
        cache: &mut HighlightCache,
        buf: &Buffer,
        ext: &str,
    ) -> Vec<(String, Color32)> {
        assert!(h.highlight(cache, buf, ext, usize::MAX, ALL));
        let font = FontId::monospace(14.0);
        let mut job = LayoutJob::default();
        for line in 0..buf.line_count() {
            let start = buf.line_start(line);
            let end = if line + 1 < buf.line_count() {
                buf.line_start(line + 1)
            } else {
                buf.len_chars()
            };
            h.append(
                &mut job,
                &buf.slice(start..end),
                0,
                cache.spans(line),
                &font,
            );
        }
        job.sections
            .iter()
            .map(|s| {
                (
                    job.text[s.byte_range.start.0..s.byte_range.end.0].to_owned(),
                    s.format.color,
                )
            })
            .collect()
    }

    /// Highlighting `after` incrementally (from `before`) must match a fresh run.
    fn assert_incremental_matches(before: &str, after: &str, ext: &str) {
        let h = highlighter();
        let mut cache = HighlightCache::default();
        let mut buf = Buffer::from(before);
        styled(&h, &mut cache, &buf, ext);
        buf.set_text(after);
        let got = styled(&h, &mut cache, &buf, ext);
        let fresh = Buffer::from(after);
        let want = styled(&h, &mut HighlightCache::default(), &fresh, ext);
        assert_eq!(got, want, "{before:?} -> {after:?}");
    }

    const RUST: &str = "fn main() {\n    let x = 1;\n    // comment\n    println!(\"{x}\");\n}\n";

    #[test]
    fn edits_match_full_rehighlight() {
        let cases = [
            // Edit in the middle.
            RUST.replace("let x = 1", "let y = 22"),
            // Open a block comment: every following line changes.
            RUST.replace("let x", "/* let x"),
            // Insert and delete lines.
            RUST.replace("    // comment\n", "    a();\n    b();\n    c();\n"),
            RUST.replace("    // comment\n", ""),
            // Edits at the very start and end.
            format!("use std::fmt;\n{RUST}"),
            format!("{RUST}fn more() {{}}"),
            RUST.trim_end().to_owned(),
            String::new(),
        ];
        for after in &cases {
            assert_incremental_matches(RUST, after, "rs");
            assert_incremental_matches(after, RUST, "rs");
        }
    }

    #[test]
    fn closing_a_comment_rehighlights_below() {
        let open = RUST.replace("let x", "/* let x");
        let closed = open.replace("// comment", "*/ // comment");
        assert_incremental_matches(&open, &closed, "rs");
    }

    #[test]
    fn changing_language_starts_over() {
        let h = highlighter();
        let mut cache = HighlightCache::default();
        let buf = Buffer::from(RUST);
        styled(&h, &mut cache, &buf, "rs");
        let got = styled(&h, &mut cache, &buf, "txt");
        let want = styled(&h, &mut HighlightCache::default(), &buf, "txt");
        assert_eq!(got, want);
    }

    #[test]
    fn highlights_only_what_is_asked_for() {
        let h = highlighter();
        let text = RUST.repeat(1000);
        let buf = Buffer::from(text.as_str());
        let mut cache = HighlightCache::default();
        assert!(h.highlight(&mut cache, &buf, "rs", 50, ALL));
        assert_eq!(cache.highlighted(), 50);
        assert!(cache.spans(49).is_some());
        assert!(cache.spans(50).is_none());
    }

    #[test]
    fn edit_far_above_reuses_lines_below() {
        let h = highlighter();
        let text = RUST.repeat(200);
        let mut buf = Buffer::from(text.as_str());
        let mut cache = HighlightCache::default();
        assert!(h.highlight(&mut cache, &buf, "rs", usize::MAX, ALL));
        let lines = buf.line_count();
        // Typing on line 2 leaves the parser in the same state after it, so
        // the rest is reused: re-highlighting it all takes ~1 line of work.
        let at = buf.line_start(1) + 4;
        buf.edit([(at..at, "z")], EditKind::Typing, None);
        h.sync(&mut cache, &buf, "rs");
        assert_eq!(cache.valid, 1);
        assert_eq!(cache.window_end, lines);
        assert!(h.highlight(&mut cache, &buf, "rs", lines, Duration::ZERO));
    }

    #[test]
    fn multi_line_edits_follow_the_log() {
        let h = highlighter();
        let mut buf = Buffer::from(RUST);
        let mut cache = HighlightCache::default();
        styled(&h, &mut cache, &buf, "rs");
        // Several edits between frames, some adding and removing lines.
        buf.apply_edits([(0..0, "/*\n")]);
        buf.apply_edits([(5..5, "x\ny\n")]);
        let end = buf.len_chars();
        buf.apply_edits([(end - 2..end, "*/")]);
        let got = styled(&h, &mut cache, &buf, "rs");
        let fresh = Buffer::from(buf.as_str());
        let want = styled(&h, &mut HighlightCache::default(), &fresh, "rs");
        assert_eq!(got, want);
    }

    #[test]
    fn resolves_semantic_tokens_through_theme_scopes() {
        let mut h = highlighter();
        let function: Arc<str> = Arc::from("function");
        let style = h
            .semantic_style(&function, 0)
            .expect("theme colors functions");
        assert!(style.color.is_some());
        assert!(!style.strikethrough);
        // Comments are left to syntax highlighting.
        assert_eq!(h.semantic_style(&Arc::from("comment"), 0), None);
        // Unknown kinds only pick up the deprecated strikethrough.
        let unknown: Arc<str> = Arc::from("somethingNew");
        assert_eq!(h.semantic_style(&unknown, 0), None);
        let old = h.semantic_style(&unknown, token_mods::DEPRECATED).unwrap();
        assert!(old.strikethrough && old.color.is_none());
        let generation = h.generation();
        h.set_theme(h.theme.clone());
        assert_ne!(h.generation(), generation);
    }
}

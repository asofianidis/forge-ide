//! Syntax highlighting via `syntect`, converted into egui `LayoutJob`s.
//!
//! Highlighting is incremental: the parser state at the start of every line is
//! kept, so after an edit only the changed lines are re-parsed, plus any lines
//! below them whose start state changed as a result (e.g. after opening a block
//! comment). Typing in a large file costs about one line of parsing, not the
//! whole file.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter as ThemeHighlighter, Style, Theme,
};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};
use syntect::util::LinesWithEndings;

use super::decorate::{StyleOverride, semantic_scopes};
use crate::lsp::features::token_mods;

/// Parser and highlighter state at the start of a line.
#[derive(Clone, PartialEq, Eq)]
struct LineState {
    parse: ParseState,
    highlight: HighlightState,
}

/// One highlighted line of the last input.
struct Line {
    /// The line's text, including its line ending.
    text: String,
    /// State at the start of the line.
    start: LineState,
    /// Styled byte lengths covering `text`, or `None` if parsing failed.
    spans: Option<Vec<(Style, usize)>>,
}

/// Highlighting of the last input, so it can be reused after an edit.
struct Cached {
    extension: String,
    text: String,
    lines: Vec<Line>,
    /// State after the last line.
    end: LineState,
    font: FontId,
    job: LayoutJob,
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
    cache: Option<Cached>,
    /// Theme style of each semantic token type and modifiers seen so far.
    semantic: HashMap<(Arc<str>, u8), Option<StyleOverride>>,
    /// Bumped on every theme change, so styles derived from it are redone.
    generation: u64,
}

impl Highlighter {
    pub fn new(theme: Theme) -> Self {
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme,
            cache: None,
            semantic: HashMap::new(),
            generation: 0,
        }
    }

    /// Switches the color theme, invalidating cached highlighting.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.cache = None;
        self.semantic.clear();
        self.generation += 1;
    }

    /// Changes whenever the theme does.
    pub fn generation(&self) -> u64 {
        self.generation
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

    pub fn highlight(&mut self, text: &str, extension: &str, font: FontId) -> LayoutJob {
        let reusable = self.cache.take().filter(|c| c.extension == extension);
        if let Some(c) = &reusable
            && c.text == text
            && c.font == font
        {
            let job = c.job.clone();
            self.cache = reusable;
            return job;
        }

        let (old_lines, old_end) = match reusable {
            Some(c) => (c.lines, Some(c.end)),
            None => (Vec::new(), None),
        };
        let (lines, end) = self.highlight_lines(text, extension, old_lines, old_end);
        let job = self.build_job(&lines, &font);
        self.cache = Some(Cached {
            extension: extension.to_owned(),
            text: text.to_owned(),
            lines,
            end,
            font,
            job: job.clone(),
        });
        job
    }

    /// Highlights `text`, reusing lines from the previous input where possible.
    fn highlight_lines(
        &self,
        text: &str,
        extension: &str,
        mut old: Vec<Line>,
        old_end: Option<LineState>,
    ) -> (Vec<Line>, LineState) {
        let new: Vec<&str> = LinesWithEndings::from(text).collect();

        // Lines shared with the previous input at the start and end. Unchanged
        // leading lines keep their highlighting outright; unchanged trailing
        // lines do once parsing reaches them in the same state as before.
        let prefix = old
            .iter()
            .zip(&new)
            .take_while(|(o, n)| o.text == **n)
            .count();
        let max_suffix = old.len().min(new.len()) - prefix;
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(max_suffix)
            .take_while(|(o, n)| o.text == **n)
            .count();

        let mut state = match (old.get(prefix), &old_end) {
            (Some(line), _) => line.start.clone(),
            (None, Some(end)) if prefix > 0 => end.clone(),
            _ => self.initial_state(extension),
        };
        let mut tail = old.split_off(prefix);
        let mut lines = old;
        lines.reserve(new.len() - prefix);

        let theme = ThemeHighlighter::new(&self.theme);
        let resume_at = new.len() - suffix;
        for (idx, &line) in new.iter().enumerate().skip(prefix) {
            if idx >= resume_at {
                let old_idx = tail.len() - (new.len() - idx);
                if tail[old_idx].start == state {
                    // Everything from here down is unchanged.
                    lines.extend(tail.drain(old_idx..));
                    let end = old_end.expect("a non-empty old input has an end state");
                    return (lines, end);
                }
            }
            let start = state.clone();
            let spans = self.highlight_line(line, &mut state, &theme);
            lines.push(Line {
                text: line.to_owned(),
                start,
                spans,
            });
        }
        (lines, state)
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

    fn build_job(&self, lines: &[Line], font: &FontId) -> LayoutJob {
        let mut job = LayoutJob::default();
        for line in lines {
            let Some(spans) = &line.spans else {
                job.append(
                    &line.text,
                    0.0,
                    TextFormat::simple(font.clone(), self.foreground()),
                );
                continue;
            };
            let mut offset = 0;
            for (style, len) in spans {
                let piece = &line.text[offset..offset + len];
                offset += len;
                let fg = style.foreground;
                let mut format =
                    TextFormat::simple(font.clone(), Color32::from_rgb(fg.r, fg.g, fg.b));
                format.italics = style.font_style.contains(FontStyle::ITALIC);
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    format.underline = Stroke::new(1.0, format.color);
                }
                job.append(piece, 0.0, format);
            }
        }
        job
    }

    fn foreground(&self) -> Color32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn highlighter() -> Highlighter {
        let themes = syntect::highlighting::ThemeSet::load_defaults();
        Highlighter::new(themes.themes["base16-ocean.dark"].clone())
    }

    fn font() -> FontId {
        FontId::monospace(14.0)
    }

    /// Text and color of every section, for comparing jobs.
    fn sections(job: &LayoutJob) -> Vec<(String, Color32)> {
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
        let mut incremental = highlighter();
        incremental.highlight(before, ext, font());
        let got = incremental.highlight(after, ext, font());
        let want = highlighter().highlight(after, ext, font());
        assert_eq!(got.text, after);
        assert_eq!(sections(&got), sections(&want), "{before:?} -> {after:?}");
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
    fn font_change_rebuilds_cached_job() {
        let mut h = highlighter();
        let small = h.highlight(RUST, "rs", FontId::monospace(10.0));
        let large = h.highlight(RUST, "rs", FontId::monospace(20.0));
        assert!(small.sections.iter().all(|s| s.format.font_id.size == 10.0));
        assert!(large.sections.iter().all(|s| s.format.font_id.size == 20.0));
        assert_eq!(sections(&small), sections(&large));
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

    #[test]
    fn changing_language_starts_over() {
        let mut h = highlighter();
        h.highlight(RUST, "rs", font());
        let got = h.highlight(RUST, "txt", font());
        let want = highlighter().highlight(RUST, "txt", font());
        assert_eq!(sections(&got), sections(&want));
    }
}

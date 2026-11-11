//! Syntax highlighting via `syntect`, converted into egui `LayoutJob`s.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::settings::DEFAULT_THEME;

/// Last highlighted input, so unchanged text isn't re-highlighted every frame.
struct Cached {
    extension: String,
    font: FontId,
    text: String,
    job: LayoutJob,
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
    theme_name: String,
    cache: Option<Cached>,
}

impl Default for Highlighter {
    fn default() -> Self {
        let themes = ThemeSet::load_defaults();
        assert!(themes.themes.contains_key(DEFAULT_THEME));
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            themes,
            theme_name: DEFAULT_THEME.to_owned(),
            cache: None,
        }
    }
}

impl Highlighter {
    /// Names of the available themes, sorted.
    pub fn theme_names(&self) -> impl Iterator<Item = &str> {
        // `ThemeSet::themes` is a BTreeMap, so this is already sorted.
        self.themes.themes.keys().map(String::as_str)
    }

    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// Switches theme. Returns false (keeping the current theme) if `name` is unknown.
    pub fn set_theme(&mut self, name: &str) -> bool {
        if !self.themes.themes.contains_key(name) {
            return false;
        }
        if self.theme_name != name {
            self.theme_name = name.to_owned();
            self.cache = None;
        }
        true
    }

    fn theme(&self) -> &Theme {
        &self.themes.themes[&self.theme_name]
    }

    /// Whether the current theme has a dark background.
    pub fn is_dark(&self) -> bool {
        let bg = self.background();
        // Rec. 601 luma.
        let luma =
            0.299 * f32::from(bg.r()) + 0.587 * f32::from(bg.g()) + 0.114 * f32::from(bg.b());
        luma < 128.0
    }

    pub fn highlight(&mut self, text: &str, extension: &str, font: FontId) -> LayoutJob {
        if let Some(c) = &self.cache
            && c.extension == extension
            && c.font == font
            && c.text == text
        {
            return c.job.clone();
        }
        let job = self.build_job(text, extension, font.clone());
        self.cache = Some(Cached {
            extension: extension.to_owned(),
            font,
            text: text.to_owned(),
            job: job.clone(),
        });
        job
    }

    fn build_job(&self, text: &str, extension: &str, font: FontId) -> LayoutJob {
        let syntax = self
            .syntaxes
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
        let mut hl = HighlightLines::new(syntax, self.theme());
        let mut job = LayoutJob::default();

        for line in LinesWithEndings::from(text) {
            let Ok(ranges) = hl.highlight_line(line, &self.syntaxes) else {
                job.append(
                    line,
                    0.0,
                    TextFormat::simple(font.clone(), Color32::LIGHT_GRAY),
                );
                continue;
            };
            for (style, piece) in ranges {
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

    pub fn background(&self) -> Color32 {
        self.theme()
            .settings
            .background
            .map(|c| Color32::from_rgb(c.r, c.g, c.b))
            .unwrap_or(Color32::from_gray(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_between_bundled_themes() {
        let mut hl = Highlighter::default();
        assert!(hl.is_dark());
        assert!(hl.theme_names().any(|n| n == "InspiredGitHub"));
        assert!(hl.set_theme("InspiredGitHub"));
        assert_eq!(hl.theme_name(), "InspiredGitHub");
        assert!(!hl.is_dark());
        assert!(!hl.set_theme("no such theme"));
        assert_eq!(hl.theme_name(), "InspiredGitHub");
    }

    #[test]
    fn font_change_invalidates_cache() {
        let mut hl = Highlighter::default();
        let small = hl.highlight("fn x() {}", "rs", FontId::monospace(10.0));
        let large = hl.highlight("fn x() {}", "rs", FontId::monospace(20.0));
        assert_eq!(small.sections[0].format.font_id.size, 10.0);
        assert_eq!(large.sections[0].format.font_id.size, 20.0);
    }
}

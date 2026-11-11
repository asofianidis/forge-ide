//! Syntax highlighting via `syntect`, converted into egui `LayoutJob`s.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Last highlighted input, so unchanged text isn't re-highlighted every frame.
struct Cached {
    extension: String,
    font: FontId,
    text: String,
    job: LayoutJob,
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
    cache: Option<Cached>,
}

impl Highlighter {
    pub fn new(theme: Theme) -> Self {
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme,
            cache: None,
        }
    }

    /// Switches the color theme, invalidating cached highlighting.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.cache = None;
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
        let mut hl = HighlightLines::new(syntax, &self.theme);
        let mut job = LayoutJob::default();

        for line in LinesWithEndings::from(text) {
            let Ok(ranges) = hl.highlight_line(line, &self.syntaxes) else {
                job.append(
                    line,
                    0.0,
                    TextFormat::simple(font.clone(), self.foreground()),
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
    use crate::theme;

    #[test]
    fn theme_change_invalidates_cache() {
        let mut hl = Highlighter::new(theme::Theme::blossom().syntect_theme());
        let dark = hl.highlight("fn x() {}", "rs", FontId::monospace(14.0));
        hl.set_theme(theme::Theme::blossom_light().syntect_theme());
        let light = hl.highlight("fn x() {}", "rs", FontId::monospace(14.0));
        assert_ne!(
            dark.sections[0].format.color,
            light.sections[0].format.color
        );
        assert_ne!(
            hl.background(),
            Highlighter::new(theme::Theme::blossom().syntect_theme()).background()
        );
    }

    #[test]
    fn font_change_invalidates_cache() {
        let mut hl = Highlighter::new(theme::Theme::blossom().syntect_theme());
        let small = hl.highlight("fn x() {}", "rs", FontId::monospace(10.0));
        let large = hl.highlight("fn x() {}", "rs", FontId::monospace(20.0));
        assert_eq!(small.sections[0].format.font_id.size, 10.0);
        assert_eq!(large.sections[0].format.font_id.size, 20.0);
    }
}

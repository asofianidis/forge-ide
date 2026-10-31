//! Syntax highlighting via `syntect`, converted into egui `LayoutJob`s.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const THEME_NAME: &str = "base16-ocean.dark";

/// Last highlighted input, so unchanged text isn't re-highlighted every frame.
struct Cached {
    extension: String,
    text: String,
    job: LayoutJob,
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
    cache: Option<Cached>,
}

impl Default for Highlighter {
    fn default() -> Self {
        let mut themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .remove(THEME_NAME)
            .expect("theme is bundled with syntect");
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme,
            cache: None,
        }
    }
}

impl Highlighter {
    pub fn highlight(&mut self, text: &str, extension: &str, font: FontId) -> LayoutJob {
        if let Some(c) = &self.cache
            && c.extension == extension
            && c.text == text
        {
            return c.job.clone();
        }
        let job = self.build_job(text, extension, font);
        self.cache = Some(Cached {
            extension: extension.to_owned(),
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
        self.theme
            .settings
            .background
            .map(|c| Color32::from_rgb(c.r, c.g, c.b))
            .unwrap_or(Color32::from_gray(30))
    }
}

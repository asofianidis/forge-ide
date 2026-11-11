//! Command palette: a fuzzy-filtered list of [`Command`]s.

use egui::text::{LayoutJob, TextFormat};
use egui::{Key, Modifiers};

use crate::commands::Command;

/// Scores how well `query` fuzzy-matches `candidate` (higher is better).
///
/// Every query character must appear in order (case-insensitively). Returns the
/// score and the char indices of the matched characters, or `None` if it doesn't match.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<(i32, Vec<usize>)> {
    let mut positions = Vec::new();
    let mut score = 0;
    let mut prev: Option<usize> = None;
    let chars: Vec<char> = candidate.chars().collect();
    let mut ci = 0;
    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let q = q.to_lowercase().next().unwrap_or(q);
        let found = (ci..chars.len()).find(|&i| chars[i].to_lowercase().next() == Some(q))?;
        let at_word_start = found == 0 || !chars[found - 1].is_alphanumeric();
        score += 1;
        if prev.is_some_and(|p| p + 1 == found) {
            score += 5; // consecutive run
        }
        if at_word_start {
            score += 8;
        }
        score -= (found - ci).min(10) as i32; // penalise gaps
        positions.push(found);
        prev = Some(found);
        ci = found + 1;
    }
    // Prefer shorter candidates among equal matches.
    Some((score * 100 - chars.len() as i32, positions))
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    query: String,
    selected: usize,
    focus: bool,
}

impl Palette {
    pub fn show(&mut self) {
        self.open = true;
        self.focus = true;
        self.query.clear();
        self.selected = 0;
    }

    /// Commands matching the current query, best first, with matched char positions.
    fn results(&self) -> Vec<(Command, Vec<usize>)> {
        let mut scored: Vec<_> = Command::ALL
            .iter()
            .filter(|&&c| c != Command::ShowPalette)
            .filter_map(|&c| fuzzy_match(&self.query, c.label()).map(|(s, pos)| (s, c, pos)))
            .collect();
        if !self.query.trim().is_empty() {
            scored.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
        }
        scored.into_iter().map(|(_, c, pos)| (c, pos)).collect()
    }

    /// Shows the palette. Returns the command to run once one is chosen.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Command> {
        if !self.open {
            return None;
        }
        let results = self.results();
        self.selected = self.selected.min(results.len().saturating_sub(1));

        // Handle navigation before the text field sees the arrow keys.
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if up {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or(results.len().saturating_sub(1));
        }
        if down {
            self.selected = if self.selected + 1 >= results.len() {
                0
            } else {
                self.selected + 1
            };
        }
        let mut chosen = if enter {
            results.get(self.selected).map(|(c, _)| *c)
        } else {
            None
        };

        let mut close = escape;
        egui::Window::new("Command Palette")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 48.0])
            .fixed_size([480.0, 0.0])
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Type a command")
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.focus) {
                    response.request_focus();
                }
                if response.changed() {
                    self.selected = 0;
                }
                // Clicking elsewhere dismisses the palette.
                if response.lost_focus() && !enter {
                    close = true;
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        if results.is_empty() {
                            ui.weak("No matching commands");
                        }
                        for (i, (cmd, positions)) in results.iter().enumerate() {
                            let selected = i == self.selected;
                            let row = ui.horizontal(|ui| {
                                let label = highlighted(ui, cmd.label(), positions);
                                let r = ui.selectable_label(selected, label);
                                if let Some(s) = cmd.shortcut() {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| ui.weak(ctx.format_shortcut(&s)),
                                    );
                                }
                                r
                            });
                            if selected && (up || down) {
                                row.inner.scroll_to_me(None);
                            }
                            if row.inner.clicked() {
                                chosen = Some(*cmd);
                            }
                        }
                    });
            });

        if chosen.is_some() || close {
            self.open = false;
        }
        chosen
    }
}

/// Renders `text` with the chars at `positions` emphasised.
fn highlighted(ui: &egui::Ui, text: &str, positions: &[usize]) -> LayoutJob {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let normal = TextFormat::simple(font.clone(), ui.visuals().text_color());
    let strong = TextFormat::simple(font, ui.visuals().strong_text_color());
    let accent = TextFormat {
        underline: egui::Stroke::new(1.0, ui.visuals().strong_text_color()),
        ..strong
    };
    let mut job = LayoutJob::default();
    let mut buf = [0; 4];
    for (i, c) in text.chars().enumerate() {
        let format = if positions.contains(&i) {
            accent.clone()
        } else {
            normal.clone()
        };
        job.append(c.encode_utf8(&mut buf), 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_chars_in_order() {
        assert!(fuzzy_match("sv", "File: Save").is_some());
        assert!(fuzzy_match("vs", "File: Save").is_none());
        assert!(fuzzy_match("", "anything").is_some());
    }

    #[test]
    fn reports_match_positions() {
        let (_, pos) = fuzzy_match("fsa", "File: Save").unwrap();
        assert_eq!(pos, vec![0, 6, 7]);
    }

    #[test]
    fn prefers_word_starts_and_runs() {
        let score = |q, c| fuzzy_match(q, c).unwrap().0;
        assert!(score("save", "File: Save") > score("save", "View: Some Absurd Verbose Entry"));
        assert!(score("gtl", "Go: Go to Line…") > score("gtl", "Edit: Light Toll"));
    }

    #[test]
    fn case_insensitive() {
        assert!(fuzzy_match("UNDO", "Edit: Undo").is_some());
    }

    #[test]
    fn palette_lists_commands_without_itself() {
        let palette = Palette::default();
        let results = palette.results();
        assert_eq!(results.len(), Command::ALL.len() - 1);
        assert!(results.iter().all(|(c, _)| *c != Command::ShowPalette));
    }
}

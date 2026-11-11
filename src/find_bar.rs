//! Find / replace bar shown above the editor, and the go-to-line prompt.

use std::ops::Range;

use egui::{Key, TextBuffer as _};

use crate::editor::Document;
use crate::editor::search::{Matcher, Query, parse_line_col};

pub enum FindAction {
    Next,
    Prev,
    Replace,
    ReplaceAll,
    Close,
}

/// Matches for one (document revision, query) pair.
struct Cache {
    doc: egui::Id,
    revision: u64,
    query: Query,
    matcher: Option<Matcher>,
    /// Byte ranges into the document text, for regex replacement.
    byte_ranges: Vec<Range<usize>>,
    /// The same matches as char ranges, for cursor placement and highlighting.
    char_ranges: Vec<Range<usize>>,
    error: Option<String>,
}

#[derive(Default)]
pub struct FindBar {
    pub open: bool,
    show_replace: bool,
    query: Query,
    replace_with: String,
    focus_query: bool,
    cache: Option<Cache>,
    /// Index into the current matches of the highlighted one.
    pub current: Option<usize>,
}

impl FindBar {
    pub fn show(&mut self, replace: bool, seed: Option<String>) {
        self.open = true;
        self.show_replace = replace;
        self.focus_query = true;
        if let Some(seed) = seed.filter(|s| !s.is_empty() && !s.contains('\n')) {
            self.query.pattern = seed;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.current = None;
    }

    pub fn query_focus_id(&self) -> egui::Id {
        egui::Id::unique("find_bar_query")
    }

    /// Recomputes matches if the document or query changed.
    ///
    /// Returns true when the query itself changed (as opposed to the document text).
    pub fn refresh(&mut self, doc: &Document) -> bool {
        if let Some(c) = &self.cache
            && c.doc == doc.id
            && c.revision == doc.buffer.revision()
            && c.query == self.query
        {
            return false;
        }
        let query_changed = self.cache.as_ref().is_none_or(|c| c.query != self.query);
        let text = doc.buffer.as_str();
        let (matcher, error) = match Matcher::new(&self.query) {
            Ok(m) => (m, None),
            Err(e) => (None, Some(e.to_string())),
        };
        let byte_ranges = matcher
            .as_ref()
            .map(|m| m.find_all(text))
            .unwrap_or_default();
        let char_ranges = byte_ranges
            .iter()
            .map(|r| doc.buffer.byte_to_char_range(r.clone()))
            .collect();
        self.cache = Some(Cache {
            doc: doc.id,
            revision: doc.buffer.revision(),
            query: self.query.clone(),
            matcher,
            byte_ranges,
            char_ranges,
            error,
        });
        self.current = None;
        query_changed
    }

    /// Char ranges of all matches (empty until [`Self::refresh`] runs).
    pub fn matches(&self) -> &[Range<usize>] {
        self.cache.as_ref().map_or(&[], |c| &c.char_ranges)
    }

    /// Replacement edits (char range, new text) for the given match indices.
    pub fn replacements(
        &self,
        doc: &Document,
        indices: impl IntoIterator<Item = usize>,
    ) -> Vec<(Range<usize>, String)> {
        let Some(cache) = &self.cache else {
            return Vec::new();
        };
        let Some(matcher) = &cache.matcher else {
            return Vec::new();
        };
        let text = doc.buffer.as_str();
        indices
            .into_iter()
            .filter_map(|i| {
                let bytes = cache.byte_ranges.get(i)?.clone();
                let new = matcher.replacement(text, bytes, &self.replace_with);
                Some((cache.char_ranges[i].clone(), new))
            })
            .collect()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<FindAction> {
        let mut action = None;
        let query_id = self.query_focus_id();
        ui.horizontal(|ui| {
            let toggle = if self.show_replace { "⏷" } else { "⏵" };
            if ui
                .small_button(toggle)
                .on_hover_text("Toggle replace")
                .clicked()
            {
                self.show_replace = !self.show_replace;
            }
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.query.pattern)
                    .id(query_id)
                    .hint_text("Find")
                    .desired_width(240.0),
            );
            if std::mem::take(&mut self.focus_query) {
                response.request_focus();
                select_all(ui.ctx(), query_id, &self.query.pattern);
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                action = Some(if ui.input(|i| i.modifiers.shift) {
                    FindAction::Prev
                } else {
                    FindAction::Next
                });
                response.request_focus();
            }
            ui.toggle_value(&mut self.query.case_sensitive, "Aa")
                .on_hover_text("Match case");
            ui.toggle_value(&mut self.query.regex, ".*")
                .on_hover_text("Use regular expression");

            match self.cache.as_ref() {
                Some(Cache { error: Some(e), .. }) => {
                    ui.colored_label(ui.visuals().error_fg_color, "Invalid regex")
                        .on_hover_text(e);
                }
                _ if self.query.pattern.is_empty() => {}
                _ => {
                    let total = self.matches().len();
                    let label = match self.current {
                        Some(i) if total > 0 => format!("{} of {total}", i + 1),
                        _ if total == 0 => "No results".to_owned(),
                        _ => format!("{total} results"),
                    };
                    ui.label(label);
                }
            }
            if ui
                .small_button("↑")
                .on_hover_text("Previous (Shift+Enter)")
                .clicked()
            {
                action = Some(FindAction::Prev);
            }
            if ui.small_button("↓").on_hover_text("Next (Enter)").clicked() {
                action = Some(FindAction::Next);
            }
            if ui.small_button("×").on_hover_text("Close (Esc)").clicked() {
                action = Some(FindAction::Close);
            }
        });
        if self.show_replace {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().interact_size.y + ui.spacing().item_spacing.x);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.replace_with)
                        .hint_text("Replace")
                        .desired_width(240.0),
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    action = Some(FindAction::Replace);
                    response.request_focus();
                }
                if ui.button("Replace").clicked() {
                    action = Some(FindAction::Replace);
                }
                if ui.button("Replace All").clicked() {
                    action = Some(FindAction::ReplaceAll);
                }
            });
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            action = Some(FindAction::Close);
        }
        action
    }
}

/// Go-to-line prompt state.
#[derive(Default)]
pub struct GotoLine {
    pub open: bool,
    input: String,
    focus: bool,
}

impl GotoLine {
    pub fn show(&mut self) {
        self.open = true;
        self.focus = true;
        self.input.clear();
    }

    /// Shows the prompt. Returns the 1-based (line, column) to jump to once confirmed.
    pub fn ui(&mut self, ctx: &egui::Context, line_count: usize) -> Option<(usize, usize)> {
        if !self.open {
            return None;
        }
        let mut target = None;
        let mut close = false;
        egui::Window::new("Go to Line")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 48.0])
            .show(ctx, |ui| {
                let parsed = parse_line_col(&self.input);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .hint_text(format!("Line[:column] (1–{line_count})"))
                        .desired_width(260.0),
                );
                if std::mem::take(&mut self.focus) {
                    response.request_focus();
                }
                if !self.input.trim().is_empty() && parsed.is_none() {
                    ui.colored_label(ui.visuals().error_fg_color, "Enter a line number");
                }
                if response.lost_focus() {
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        target = parsed.map(|(line, col)| (line, col.unwrap_or(1)));
                    }
                    close = true;
                }
            });
        if close || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.open = false;
        }
        target
    }
}

/// Selects the whole contents of a single-line text field.
fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(0),
        CCursor::new(text.chars().count()),
    )));
    egui::TextEdit::store_state(ctx, id, state);
}

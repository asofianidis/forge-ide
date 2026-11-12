//! The find / replace bar above the editor.

use std::hash::{DefaultHasher, Hash, Hasher};

use egui::{Key, Modifiers, RichText};
use regex::Regex;

use super::{ForgeApp, toggle};
use crate::editor::find::{self, FindOptions, Match};
use crate::editor::ops::Selection;

#[derive(Default)]
pub(super) struct FindBar {
    pub open: bool,
    pub show_replace: bool,
    pub query: String,
    pub replacement: String,
    pub options: FindOptions,
    pub matches: Vec<Match>,
    /// Index into `matches` of the highlighted match.
    pub current: Option<usize>,
    pub error: Option<String>,
    regex: Option<Regex>,
    /// What `matches` was computed from: (document id, text hash, query, options).
    computed_for: Option<(u64, u64, String, FindOptions)>,
    focus: Option<Field>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Query,
    Replace,
}

fn text_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl ForgeApp {
    pub(super) fn open_find(&mut self, ctx: &egui::Context, replace: bool) {
        let sel = self.selection(ctx);
        if let Some(doc) = self.documents.get(self.active)
            && !sel.is_empty()
        {
            let selected: String = doc
                .text()
                .chars()
                .skip(sel.start)
                .take(sel.end - sel.start)
                .collect();
            if !selected.contains('\n') {
                self.find.query = if self.find.options.regex {
                    regex::escape(&selected)
                } else {
                    selected
                };
            }
        }
        self.find.open = true;
        self.find.show_replace = replace;
        self.find.focus = Some(if replace && !self.find.query.is_empty() {
            Field::Replace
        } else {
            Field::Query
        });
        // Recompute and jump to the first match from the cursor.
        self.find.computed_for = None;
    }

    pub(super) fn close_find(&mut self, ctx: &egui::Context) {
        self.find.open = false;
        if let Some(id) = self.active_editor_id() {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    /// Recomputes matches when the query, options or text changed. When the
    /// query or options changed, the first match after the cursor is selected.
    fn refresh_matches(&mut self, ctx: &egui::Context) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let hash = text_hash(doc.text());
        let key = (doc.id, hash, self.find.query.clone(), self.find.options);
        let previous = self.find.computed_for.replace(key.clone());
        if previous.as_ref() == Some(&key) {
            return;
        }
        let query_changed = previous
            .as_ref()
            .is_none_or(|(id, _, q, o)| (*id, q, o) != (key.0, &key.2, &key.3));

        self.find.matches.clear();
        self.find.current = None;
        self.find.error = None;
        self.find.regex = None;
        if self.find.query.is_empty() {
            return;
        }
        let re = match find::build_regex(&self.find.query, self.find.options) {
            Ok(re) => re,
            Err(e) => {
                self.find.error = Some(e);
                return;
            }
        };
        self.find.matches = find::find_all(doc.text(), &re);
        self.find.regex = Some(re);
        let from = self.selection(ctx).start;
        self.find.current = self
            .find
            .matches
            .iter()
            .position(|m| m.chars.start >= from)
            .or((!self.find.matches.is_empty()).then_some(0));
        if query_changed {
            self.reveal_current(ctx);
        }
    }

    /// Selects the current match in the editor without taking focus.
    fn reveal_current(&mut self, ctx: &egui::Context) {
        if let Some(m) = self.find.current.and_then(|i| self.find.matches.get(i)) {
            let sel = Selection::new(m.chars.start, m.chars.end);
            self.set_selection(ctx, sel, false);
        }
    }

    pub(super) fn find_step(&mut self, ctx: &egui::Context, forward: bool) {
        let n = self.find.matches.len();
        if n == 0 {
            return;
        }
        self.find.current = Some(match self.find.current {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        });
        self.reveal_current(ctx);
    }

    fn replace_current(&mut self, ctx: &egui::Context) {
        let (Some(i), Some(re)) = (self.find.current, &self.find.regex) else {
            return;
        };
        let Some(m) = self.find.matches.get(i) else {
            return;
        };
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let (text, after) = find::replace_one(
            doc.text(),
            re,
            m,
            &self.find.replacement,
            self.find.options.regex,
        );
        doc.set_text(&text);
        // Continue from just after the replacement.
        self.set_selection(ctx, Selection::caret(after), false);
        self.find.computed_for = None;
        self.refresh_matches(ctx);
        self.reveal_current(ctx);
    }

    fn replace_all(&mut self, ctx: &egui::Context) {
        let Some(re) = &self.find.regex else {
            return;
        };
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let (text, count) = find::replace_all(
            doc.text(),
            re,
            &self.find.replacement,
            self.find.options.regex,
        );
        if count > 0 {
            doc.set_text(&text);
            let caret = self.selection(ctx).start;
            let len = self.documents[self.active].text().chars().count();
            self.set_selection(ctx, Selection::caret(caret.min(len)), false);
        }
        self.status = match count {
            0 => "No matches to replace".to_owned(),
            1 => "Replaced 1 occurrence".to_owned(),
            n => format!("Replaced {n} occurrences"),
        };
        self.find.computed_for = None;
    }

    /// Draws the find bar, if open.
    pub(super) fn find_bar(&mut self, ui: &mut egui::Ui) {
        if !self.find.open {
            return;
        }
        let ctx = ui.ctx().clone();
        self.refresh_matches(&ctx);

        let (f3, shift_f3) = ctx.input_mut(|i| {
            let shift_f3 = i.consume_key(Modifiers::SHIFT, Key::F3);
            (
                !shift_f3 && i.consume_key(Modifiers::NONE, Key::F3),
                shift_f3,
            )
        });
        if f3 || shift_f3 {
            self.find_step(&ctx, f3);
        }

        let muted = self.current_theme().ui.text_muted.0;
        let field_width = 260.0;
        let mut close = false;
        let mut step = None;
        let mut replace_one = false;
        let mut replace_all = false;

        ui.horizontal(|ui| {
            let arrow = if self.find.show_replace { "⏷" } else { "⏵" };
            if ui
                .add(egui::Button::new(arrow).frame(false))
                .on_hover_text("Toggle Replace")
                .clicked()
            {
                self.find.show_replace = !self.find.show_replace;
            }
            let query_id = egui::Id::unique("find_query");
            let query = ui.add(
                egui::TextEdit::singleline(&mut self.find.query)
                    .id(query_id)
                    .hint_text("Find")
                    .desired_width(field_width),
            );
            if self.find.focus == Some(Field::Query) {
                self.find.focus = None;
                query.request_focus();
                select_all(&ctx, query_id, &self.find.query);
            }
            if query.lost_focus() {
                ctx.input(|i| {
                    if i.key_pressed(Key::Enter) {
                        step = Some(!i.modifiers.shift);
                    } else if i.key_pressed(Key::Escape) {
                        close = true;
                    }
                });
                if step.is_some() {
                    query.request_focus();
                }
            }

            let o = &mut self.find.options;
            toggle(ui, &mut o.match_case, "Aa", "Match Case");
            toggle(ui, &mut o.whole_word, "ab", "Match Whole Word");
            toggle(ui, &mut o.regex, ".*", "Use Regular Expression");

            let status = match (&self.find.error, self.find.current) {
                (Some(e), _) => RichText::new(e).color(ui.visuals().error_fg_color),
                _ if self.find.query.is_empty() => RichText::new(""),
                (None, _) if self.find.matches.is_empty() => {
                    RichText::new("No results").color(muted)
                }
                (None, current) => {
                    let total = self.find.matches.len();
                    let more = if total >= find::MAX_MATCHES { "+" } else { "" };
                    let at = current.map_or("?".to_owned(), |i| (i + 1).to_string());
                    RichText::new(format!("{at} of {total}{more}")).color(muted)
                }
            };
            ui.label(status);

            let has = !self.find.matches.is_empty();
            if ui
                .add_enabled(has, egui::Button::new("↑").frame(false))
                .on_hover_text("Previous Match (Shift+Enter)")
                .clicked()
            {
                step = Some(false);
            }
            if ui
                .add_enabled(has, egui::Button::new("↓").frame(false))
                .on_hover_text("Next Match (Enter)")
                .clicked()
            {
                step = Some(true);
            }
            if ui
                .add(egui::Button::new("×").frame(false))
                .on_hover_text("Close (Escape)")
                .clicked()
            {
                close = true;
            }
        });

        if self.find.show_replace {
            ui.horizontal(|ui| {
                // Line up with the query field, past the toggle arrow.
                ui.add_space(ui.spacing().interact_size.y + ui.spacing().item_spacing.x - 4.0);
                let replace_id = egui::Id::unique("find_replace");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.find.replacement)
                        .id(replace_id)
                        .hint_text("Replace")
                        .desired_width(field_width),
                );
                if self.find.focus == Some(Field::Replace) {
                    self.find.focus = None;
                    field.request_focus();
                    select_all(&ctx, replace_id, &self.find.replacement);
                }
                if field.lost_focus() {
                    ctx.input(|i| {
                        if i.key_pressed(Key::Enter) {
                            replace_one = true;
                        } else if i.key_pressed(Key::Escape) {
                            close = true;
                        }
                    });
                    if replace_one {
                        field.request_focus();
                    }
                }
                let has = !self.find.matches.is_empty();
                if ui
                    .add_enabled(has, egui::Button::new("Replace").small())
                    .clicked()
                {
                    replace_one = true;
                }
                if ui
                    .add_enabled(has, egui::Button::new("Replace All").small())
                    .clicked()
                {
                    replace_all = true;
                }
            });
        }
        ui.separator();

        // Options may have changed this frame.
        self.refresh_matches(&ctx);
        if let Some(forward) = step {
            self.find_step(&ctx, forward);
        }
        if replace_one {
            self.replace_current(&ctx);
        }
        if replace_all {
            self.replace_all(&ctx);
        }
        if close {
            self.close_find(&ctx);
        }
    }
}

fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(text.chars().count()),
        )));
    state.store(ctx, id);
}

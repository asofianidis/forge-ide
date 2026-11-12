//! Editing commands on the active buffer, cursor/selection access, and the
//! Go to Line dialog.

use egui::text::{CCursor, CCursorRange};
use egui::{Align2, Key, Modifiers};

use super::ForgeApp;
use crate::commands::Command;
use crate::editor::Document;
use crate::editor::ops::{self, Edit, Selection};

/// Widget id of the editor for `doc`.
pub(super) fn editor_id(doc: &Document) -> egui::Id {
    egui::Id::unique(("editor", doc.id))
}

impl ForgeApp {
    pub(super) fn active_editor_id(&self) -> Option<egui::Id> {
        self.documents.get(self.active).map(editor_id)
    }

    pub(super) fn editor_has_focus(&self, ctx: &egui::Context) -> bool {
        self.active_editor_id()
            .is_some_and(|id| ctx.memory(|m| m.has_focus(id)))
    }

    /// The active editor's selection (kept by egui even when unfocused).
    pub(super) fn selection(&self, ctx: &egui::Context) -> Selection {
        self.active_editor_id()
            .and_then(|id| egui::TextEdit::load_state(ctx, id))
            .and_then(|s| s.cursor.char_range())
            .map(|r| {
                let [a, b] = r.sorted_cursors();
                Selection::new(a.index.into(), b.index.into())
            })
            .unwrap_or(Selection::caret(0))
    }

    /// Selects `sel` in the active editor and scrolls it into view.
    pub(super) fn set_selection(&mut self, ctx: &egui::Context, sel: Selection, focus: bool) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let id = editor_id(doc);
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(sel.start),
            CCursor::new(sel.end),
        )));
        state.store(ctx, id);
        if focus {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        self.cursor = doc.line_col(sel.end);
        self.scroll_to = Some(sel.start);
    }

    fn apply_edit(&mut self, ctx: &egui::Context, edit: Edit) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        doc.text = edit.text;
        self.set_selection(ctx, edit.selection, true);
    }

    /// Runs one of the line-editing commands on the active buffer.
    pub(super) fn edit_command(&mut self, ctx: &egui::Context, cmd: Command) {
        let sel = self.selection(ctx);
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let text = doc.text.as_str();
        let edit = match cmd {
            Command::ToggleComment => match ops::comment_style(&doc.language_key()) {
                Some(style) => ops::toggle_comment(text, sel, style),
                None => {
                    self.status = format!("Don't know how to comment {} files", doc.title());
                    return;
                }
            },
            Command::Indent => ops::indent(text, sel, &ops::indent_unit(text)),
            Command::Outdent => ops::outdent(text, sel, &ops::indent_unit(text)),
            Command::DuplicateLine => ops::duplicate_lines(text, sel),
            Command::MoveLineUp | Command::MoveLineDown => {
                match ops::move_lines(text, sel, cmd == Command::MoveLineUp) {
                    Some(edit) => edit,
                    None => return,
                }
            }
            _ => return,
        };
        self.apply_edit(ctx, edit);
    }

    /// Tab indents a multi-line selection (and otherwise inserts the file's
    /// indent unit); Shift+Tab outdents. Only while the editor has focus.
    pub(super) fn handle_tab(&mut self, ctx: &egui::Context) {
        if !self.editor_has_focus(ctx) {
            return;
        }
        let (shift_tab, tab) = ctx.input_mut(|i| {
            let shift_tab = i.consume_key(Modifiers::SHIFT, Key::Tab);
            (
                shift_tab,
                !shift_tab && i.consume_key(Modifiers::NONE, Key::Tab),
            )
        });
        if shift_tab {
            self.edit_command(ctx, Command::Outdent);
        } else if tab {
            let sel = self.selection(ctx);
            let Some(doc) = self.documents.get(self.active) else {
                return;
            };
            let multi_line = doc
                .text
                .chars()
                .skip(sel.start)
                .take(sel.end - sel.start)
                .any(|c| c == '\n');
            if multi_line {
                self.edit_command(ctx, Command::Indent);
            } else {
                let edit = ops::insert(&doc.text, sel, &ops::indent_unit(&doc.text));
                self.apply_edit(ctx, edit);
            }
        }
    }

    // ---- go to line ------------------------------------------------------

    pub(super) fn open_go_to_line(&mut self) {
        self.picker.close();
        self.go_to_line = Some(String::new());
    }

    pub(super) fn go_to_line_dialog(&mut self, ctx: &egui::Context) {
        let Some(input) = &mut self.go_to_line else {
            return;
        };
        let Some(doc) = self.documents.get(self.active) else {
            self.go_to_line = None;
            return;
        };
        let line_count = doc.line_count();
        let (enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let id = egui::Id::unique("go_to_line");
        let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, [0.0, 56.0]);
        let modal = egui::Modal::new(id)
            .area(area)
            .backdrop_color(egui::Color32::from_black_alpha(60))
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.add(
                    egui::TextEdit::singleline(input)
                        .id(id.with("input"))
                        .hint_text(format!("Line (1–{line_count}), or line:column"))
                        .desired_width(f32::INFINITY),
                )
                .request_focus();
            });
        let target = parse_line_col(input);
        if escape || modal.backdrop_response.clicked() {
            self.go_to_line = None;
            return;
        }
        if enter {
            self.go_to_line = None;
            if let Some((line, col)) = target {
                let at = ops::line_col_to_char(&doc.text, line.min(line_count), col);
                self.set_selection(ctx, Selection::caret(at), true);
            }
        }
    }
}

/// Parses "12" or "12:5" (1-based). Column defaults to 1.
fn parse_line_col(input: &str) -> Option<(usize, usize)> {
    let mut parts = input.trim().splitn(2, [':', ',']);
    let line = parts.next()?.trim().parse::<usize>().ok()?.max(1);
    let col = match parts.next() {
        Some(c) if !c.trim().is_empty() => c.trim().parse::<usize>().ok()?.max(1),
        _ => 1,
    };
    Some((line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_line_and_column() {
        assert_eq!(parse_line_col("12"), Some((12, 1)));
        assert_eq!(parse_line_col(" 3:7 "), Some((3, 7)));
        assert_eq!(parse_line_col("3:"), Some((3, 1)));
        assert_eq!(parse_line_col("0"), Some((1, 1)));
        assert_eq!(parse_line_col("x"), None);
        assert_eq!(parse_line_col(""), None);
    }
}

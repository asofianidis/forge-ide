//! Editing commands on the focused editor view: cursor/selection access,
//! undo, line commands at every cursor, multi-cursor and folding commands,
//! and the Go to Line dialog. Typing itself (and its aids: auto-indent,
//! auto-closing pairs) is handled by the editor widget.

use egui::{Align2, Key, Modifiers};

use super::ForgeApp;
use crate::commands::Command;
use crate::editor::EditKind;
use crate::editor::folding::FoldSource;
use crate::editor::multi::{self, CursorEdit};
use crate::editor::ops::{self, Selection};
use crate::editor::selection::{Cursor, Selections};
use crate::editor::view::{self, Reveal};

impl ForgeApp {
    pub(super) fn active_editor_id(&self) -> Option<egui::Id> {
        self.focused_view().map(|v| v.id)
    }

    pub(super) fn editor_has_focus(&self, ctx: &egui::Context) -> bool {
        self.active_editor_id()
            .is_some_and(|id| ctx.memory(|m| m.has_focus(id)))
    }

    /// The focused view's cursors, in the current text.
    pub(super) fn selections(&self) -> Selections {
        match (self.focused_view(), self.documents.get(self.active)) {
            (Some(v), Some(doc)) => v.current_selection(&doc.buffer),
            _ => Selections::default(),
        }
    }

    /// The focused view's primary selection.
    pub(super) fn selection(&self, _ctx: &egui::Context) -> Selection {
        self.selections().primary().selection()
    }

    /// Selects `sel` in the focused view and scrolls it to the middle.
    pub(super) fn set_selection(&mut self, ctx: &egui::Context, sel: Selection, focus: bool) {
        self.set_selections(
            ctx,
            Selections::single(Cursor::new(sel.start, sel.end)),
            Reveal::Center,
            focus,
        );
    }

    pub(super) fn set_selections(
        &mut self,
        ctx: &egui::Context,
        sel: Selections,
        reveal: Reveal,
        focus: bool,
    ) {
        let Some((doc, view)) = self.focused_doc_view() else {
            return;
        };
        view.set_selections(&doc.buffer, sel, reveal);
        let id = view.id;
        let head = view.sel.primary().head;
        self.cursor = doc.line_col(head);
        if focus {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    /// Undoes (or with `redo`, redoes) the last step in the active document
    /// and restores the cursors it had.
    pub(super) fn undo(&mut self, ctx: &egui::Context, redo: bool) {
        if self.pane_shown() {
            return;
        }
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let restore = if redo {
            doc.buffer.redo()
        } else {
            doc.buffer.undo()
        };
        match restore {
            Some(snapshot) => {
                let sel = Selections::from_snapshot(&snapshot);
                self.set_selections(ctx, sel, Reveal::Cursor, true);
            }
            None => {
                self.status = if redo {
                    "Nothing to redo"
                } else {
                    "Nothing to undo"
                }
                .to_owned();
            }
        }
    }

    /// Runs one of the line-editing commands at every cursor.
    pub(super) fn edit_command(&mut self, ctx: &egui::Context, cmd: Command) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let style = ops::comment_style(&doc.language_key());
        if cmd == Command::ToggleComment && style.is_none() {
            self.status = format!("Don't know how to comment {} files", doc.title());
            return;
        }
        let unit = doc.indent_unit();
        // Lines of context each command needs around a cursor.
        let (above, below) = match cmd {
            Command::MoveLineUp => (1, 0),
            Command::MoveLineDown => (0, 1),
            _ => (0, 0),
        };
        let sel = self.selections();
        let tx = multi::for_each_cursor(&doc.buffer, &sel, |b, c| {
            multi::local(b, c, above, below, |text, s| match cmd {
                Command::ToggleComment => Some(ops::toggle_comment(text, s, style?)),
                Command::Indent => Some(ops::indent(text, s, &unit)),
                Command::Outdent => Some(ops::outdent(text, s, &unit)),
                Command::DuplicateLine => Some(ops::duplicate_lines(text, s)),
                Command::MoveLineUp => ops::move_lines(text, s, true),
                Command::MoveLineDown => ops::move_lines(text, s, false),
                _ => None,
            })
            .unwrap_or(CursorEdit::Keep)
        });
        self.commit(ctx, tx);
    }

    /// Applies a multi-cursor transaction to the focused view as one undo step.
    fn commit(&mut self, ctx: &egui::Context, tx: multi::Transaction) {
        let Some((doc, view)) = self.focused_doc_view() else {
            return;
        };
        if tx.changes_text() {
            let before = view.current_selection(&doc.buffer).snapshot();
            doc.buffer.edit(
                tx.edits,
                EditKind::Command,
                Some((before, tx.after.snapshot())),
            );
        }
        self.set_selections(ctx, tx.after, Reveal::Cursor, true);
    }

    // ---- multiple cursors -----------------------------------------------

    pub(super) fn cursor_command(&mut self, ctx: &egui::Context, cmd: Command) {
        let wrap = self.settings.word_wrap;
        let Some((doc, view)) = self.focused_doc_view() else {
            return;
        };
        view.sync(&doc.buffer);
        let buf = &doc.buffer;
        let mut sel = view.sel.clone();
        match cmd {
            Command::AddCursorAbove | Command::AddCursorBelow => {
                let delta = if cmd == Command::AddCursorAbove {
                    -1
                } else {
                    1
                };
                // From the topmost (or bottom-most) cursor, like other editors.
                let from = if delta < 0 {
                    sel.as_slice()[0]
                } else {
                    sel.as_slice()[sel.len() - 1]
                };
                let cols = if wrap { view.wrap_cols() } else { None };
                let map = view.display_map(buf, cols).clone();
                let (to, goal) =
                    view::move_vertically(buf, &map, cols, from.head, from.goal, delta);
                if buf.char_to_line(to) != buf.char_to_line(from.head) {
                    let mut c = Cursor::caret(to);
                    c.goal = Some(goal);
                    sel.add(c);
                }
            }
            Command::AddNextOccurrence => {
                let p = sel.primary();
                if p.is_empty() && !sel.is_multi() {
                    // First press selects the word at the caret.
                    let w = view::word_range(buf, p.head);
                    if w.is_empty() {
                        return;
                    }
                    sel.set_primary(Cursor::new(w.start, w.end));
                } else {
                    let needle = buf.slice(p.range());
                    if needle.is_empty() {
                        return;
                    }
                    match next_occurrence(doc.text(), buf, &needle, p.end(), &sel) {
                        Some(r) => sel.add(Cursor::new(r.start, r.end)),
                        None => {
                            self.status = "No more occurrences".to_owned();
                            return;
                        }
                    }
                }
            }
            Command::SelectAllOccurrences => {
                let p = sel.primary();
                let range = if p.is_empty() {
                    view::word_range(buf, p.head)
                } else {
                    p.range()
                };
                let needle = buf.slice(range.clone());
                if needle.is_empty() {
                    return;
                }
                let text = doc.text();
                let mut cursors = Vec::new();
                let mut primary = 0;
                for (byte, m) in text.match_indices(needle.as_str()) {
                    let start = buf.rope().byte_to_char(byte);
                    let end = start + m.chars().count();
                    if start == range.start {
                        primary = cursors.len();
                    }
                    cursors.push(Cursor::new(start, end));
                }
                let n = cursors.len();
                sel = Selections::new(cursors, primary);
                self.status = format!("{n} occurrence{}", if n == 1 { "" } else { "s" });
            }
            Command::AddCursorsToLineEnds => {
                let mut cursors = Vec::new();
                for c in sel.iter() {
                    let (a, b) = (buf.char_to_line(c.start()), buf.char_to_line(c.end()));
                    if a == b {
                        cursors.push(Cursor::caret(c.end()));
                        continue;
                    }
                    for line in a..=b {
                        let end = buf.line_start(line) + buf.line_len(line);
                        // A selection ending at a line start doesn't include that line.
                        if line == b && c.end() == buf.line_start(line) {
                            continue;
                        }
                        cursors.push(Cursor::caret(end));
                    }
                }
                let n = cursors.len();
                sel = Selections::new(cursors, n.saturating_sub(1));
            }
            _ => return,
        }
        self.set_selections(ctx, sel, Reveal::Cursor, true);
    }

    // ---- folding --------------------------------------------------------

    pub(super) fn fold_command(&mut self, cmd: Command) {
        let ranges = self.fold_ranges_for(self.active);
        let Some((doc, view)) = self.focused_doc_view() else {
            return;
        };
        view.sync(&doc.buffer);
        let buf = &doc.buffer;
        let source = match &ranges {
            Some(r) => FoldSource::Ranges(r),
            None => FoldSource::Indent,
        };
        let lines: Vec<usize> = view.sel.iter().map(|c| buf.char_to_line(c.head)).collect();
        match cmd {
            Command::Fold => {
                for line in lines {
                    // The innermost region not folded yet.
                    let mut at = line;
                    while let Some((s, e)) = source.enclosing(buf, at) {
                        if !view.is_folded_at(s) {
                            view.fold(buf, s, e);
                            break;
                        }
                        if s == 0 {
                            break;
                        }
                        at = s - 1;
                    }
                }
            }
            Command::Unfold => {
                for line in lines {
                    if !view.unfold(line)
                        && let Some(f) = view
                            .folds()
                            .iter()
                            .rev()
                            .find(|f| f.start <= line && line <= f.end)
                            .copied()
                    {
                        view.unfold(f.start);
                    }
                }
            }
            Command::FoldAll => {
                for (s, e) in source.all(buf) {
                    view.fold(buf, s, e);
                }
            }
            Command::UnfoldAll => view.unfold_all(),
            _ => {}
        }
        view.reveal = Some(Reveal::Cursor);
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
                let at = doc.char_index(line.min(line_count), col);
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

/// The next match of `needle` after char `from` (wrapping around) that isn't
/// already selected.
fn next_occurrence(
    text: &str,
    buf: &crate::editor::Buffer,
    needle: &str,
    from: usize,
    sel: &Selections,
) -> Option<std::ops::Range<usize>> {
    let from_byte = buf.rope().char_to_byte(from.min(buf.len_chars()));
    let found = text[from_byte..]
        .match_indices(needle)
        .map(|(b, _)| from_byte + b)
        .chain(text[..from_byte].match_indices(needle).map(|(b, _)| b));
    let len = needle.chars().count();
    for byte in found {
        let start = buf.rope().byte_to_char(byte);
        let range = start..start + len;
        if !sel.iter().any(|c| c.range() == range) {
            return Some(range);
        }
    }
    None
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

//! Code actions (Cmd/Ctrl+.): quick fixes for diagnostics, refactors and
//! source actions offered by the language server, in a menu at the caret.

use std::ops::Range;
use std::path::{Path, PathBuf};

use egui::{Align2, Key, Modifiers, Rect, RichText};
use lsp_types::{CodeAction, CodeActionKind, CodeActionOrCommand};

use super::ForgeApp;
use crate::lsp::text::Encoding;
use crate::theme::UiColors;

pub(super) struct CodeActionMenu {
    doc: u64,
    path: PathBuf,
    /// Screen rect to show the menu under.
    anchor: Rect,
    actions: Vec<CodeActionOrCommand>,
    encoding: Encoding,
    selected: usize,
    /// Where the menu was drawn last frame, to close it on clicks elsewhere.
    rect: Option<Rect>,
}

/// A request in flight, so the reply opens the menu in the right place.
pub(super) struct CodeActionRequest {
    doc: u64,
    at: usize,
    anchor: Option<Rect>,
}

impl ForgeApp {
    /// Cmd/Ctrl+.: actions for the selection, or the caret.
    pub(super) fn quick_fix(&mut self, ctx: &egui::Context) {
        let sel = self.selection(ctx);
        self.request_code_actions(sel.start..sel.end, None);
    }

    /// Asks for code actions for `range` of the active file. The menu opens
    /// under `anchor`, or under the caret.
    pub(super) fn request_code_actions(&mut self, range: Range<usize>, anchor: Option<Rect>) {
        self.active_pane = None;
        self.language.code_actions = None;
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let Some(path) = doc.path.clone() else {
            self.status = "Save the file to use language features".to_owned();
            return;
        };
        let text = doc.text().to_owned();
        let at = range.start;
        let doc = doc.id;
        match self.lsp.code_actions(&path, &text, range) {
            Ok(()) => {
                self.language.code_action_request = Some(CodeActionRequest { doc, at, anchor })
            }
            Err(e) => self.status = e,
        }
    }

    pub(super) fn on_code_actions(
        &mut self,
        path: PathBuf,
        at: usize,
        mut actions: Vec<CodeActionOrCommand>,
        encoding: Encoding,
    ) {
        let Some(request) = self.language.code_action_request.take() else {
            return;
        };
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if doc.id != request.doc || doc.path.as_deref() != Some(&path) || request.at != at {
            return;
        }
        if actions.is_empty() {
            self.status = "No code actions available here".to_owned();
            return;
        }
        // Preferred fixes first and unavailable ones last; otherwise keep the
        // server's order, which is by relevance.
        actions.sort_by_key(|a| match a {
            CodeActionOrCommand::CodeAction(a) if a.disabled.is_some() => 2,
            CodeActionOrCommand::CodeAction(a) if a.is_preferred == Some(true) => 0,
            _ => 1,
        });
        let anchor = request.anchor.or(self.language.caret).unwrap_or_else(|| {
            Rect::from_center_size(self.egui_ctx.content_rect().center(), egui::Vec2::ZERO)
        });
        self.language.code_actions = Some(CodeActionMenu {
            doc: doc.id,
            path,
            anchor,
            actions,
            encoding,
            selected: 0,
            rect: None,
        });
    }

    /// Arrows, Enter and Escape for the open menu. Runs before shortcuts and
    /// the editor, so those keys go to the menu.
    pub(super) fn code_action_keys(&mut self, ctx: &egui::Context) {
        let Some(menu) = &mut self.language.code_actions else {
            return;
        };
        let (down, up, accept, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let n = menu.actions.len();
        if down {
            menu.selected = (menu.selected + 1) % n;
        }
        if up {
            menu.selected = (menu.selected + n - 1) % n;
        }
        if escape {
            self.language.code_actions = None;
        } else if accept {
            self.accept_code_action(ctx);
        }
    }

    /// Closes the menu when the user moves on: another tab, typing, or a
    /// click outside it.
    pub(super) fn update_code_actions(&mut self, ctx: &egui::Context, doc_id: u64, typed: bool) {
        let Some(menu) = &self.language.code_actions else {
            return;
        };
        let clicked_outside = ctx.input(|i| {
            i.pointer.primary_clicked()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| menu.rect.is_some_and(|r| !r.contains(p)))
        });
        if menu.doc != doc_id || typed || clicked_outside || self.pane_shown() {
            self.language.code_actions = None;
        }
    }

    fn accept_code_action(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.language.code_actions.take() else {
            return;
        };
        let Some(action) = menu.actions.into_iter().nth(menu.selected) else {
            return;
        };
        self.run_code_action(ctx, &menu.path, action, menu.encoding, false);
    }

    /// Applies an action's edit and runs its command, resolving it first if
    /// the server left the edit out.
    fn run_code_action(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
        action: CodeActionOrCommand,
        encoding: Encoding,
        resolved: bool,
    ) {
        let action = match action {
            CodeActionOrCommand::Command(c) => {
                self.execute_command(path, &c);
                return;
            }
            CodeActionOrCommand::CodeAction(a) => a,
        };
        if let Some(d) = &action.disabled {
            self.status = format!("{}: {}", action.title, d.reason);
            return;
        }
        if action.edit.is_none() && action.command.is_none() {
            let Some(idx) = self.doc_index(path) else {
                return;
            };
            let text = self.documents[idx].text().to_owned();
            self.status = match self.lsp.resolve_code_action(path, &text, &action) {
                Ok(true) if !resolved => format!("{}…", action.title),
                Ok(_) => format!("{}: nothing to change", action.title),
                Err(e) => e,
            };
            return;
        }
        let title = action.title.clone();
        if let Some(edit) = action.edit {
            match self.apply_workspace_edit(ctx, &title, edit, encoding) {
                Ok(_) => self.status = title,
                Err(e) => {
                    self.status = format!("Error: {e}");
                    return;
                }
            }
        }
        if let Some(c) = action.command {
            self.execute_command(path, &c);
        }
    }

    fn execute_command(&mut self, path: &Path, command: &lsp_types::Command) {
        let Some(idx) = self.doc_index(path) else {
            return;
        };
        let text = self.documents[idx].text().to_owned();
        match self.lsp.execute_command(path, &text, command) {
            Ok(()) => self.status = command.title.clone(),
            Err(e) => self.status = e,
        }
    }

    pub(super) fn on_code_action_resolved(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
        action: CodeAction,
        encoding: Encoding,
    ) {
        self.run_code_action(
            ctx,
            path,
            CodeActionOrCommand::CodeAction(action),
            encoding,
            true,
        );
    }

    pub(super) fn show_code_actions(&mut self, ctx: &egui::Context, colors: &UiColors) {
        let Some(menu) = &mut self.language.code_actions else {
            return;
        };
        let mut clicked = None;
        let response = egui::Area::new(egui::Id::unique("lsp_code_actions"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.anchor.left_bottom() + egui::vec2(0.0, 2.0))
            .pivot(Align2::LEFT_TOP)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        ui.set_min_width(260.0);
                        ui.set_max_width(560.0);
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .auto_shrink([true, true])
                            .show(ui, |ui| {
                                for (i, action) in menu.actions.iter().enumerate() {
                                    let (title, kind, disabled) = describe(action);
                                    let mut text = RichText::new(title);
                                    if disabled.is_some() {
                                        text = text.color(colors.text_muted.0);
                                    }
                                    let button = egui::Button::selectable(i == menu.selected, text)
                                        .right_text(
                                            RichText::new(kind).small().color(colors.text_muted.0),
                                        );
                                    let mut row = ui.add(button);
                                    if let Some(reason) = disabled {
                                        row = row.on_hover_text(reason);
                                    }
                                    if row.clicked() {
                                        clicked = Some(i);
                                    }
                                }
                            });
                        ui.add_space(2.0);
                        ui.weak(RichText::new("Enter to apply, Escape to dismiss").small());
                    });
            });
        menu.rect = Some(response.response.rect);
        if let Some(i) = clicked {
            menu.selected = i;
            self.accept_code_action(ctx);
        }
    }
}

/// Title, a short kind tag, and why it's unavailable, if it is.
fn describe(action: &CodeActionOrCommand) -> (&str, &'static str, Option<&str>) {
    match action {
        CodeActionOrCommand::Command(c) => (&c.title, "", None),
        CodeActionOrCommand::CodeAction(a) => {
            let kind = match a.kind.as_ref().map(CodeActionKind::as_str) {
                Some(k) if k.starts_with("quickfix") => "fix",
                Some(k) if k.starts_with("refactor") => "refactor",
                Some(k) if k.starts_with("source") => "source",
                _ => "",
            };
            let kind = if a.is_preferred == Some(true) && kind == "fix" {
                "preferred fix"
            } else {
                kind
            };
            (
                &a.title,
                kind,
                a.disabled.as_ref().map(|d| d.reason.as_str()),
            )
        }
    }
}

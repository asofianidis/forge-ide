//! Language-server features of the app: diagnostics, completion and go to definition.

use egui::TextBuffer as _;
use egui::text::CCursor;
use egui::text_edit::TextEditOutput;

use super::ForgeApp;
use crate::completion::{self, CompletionKey, CompletionPopup};
use crate::editor::{Diagnostic, Severity};
use crate::lsp::{LspEvent, char_range, normalize};
use lsp_types::DiagnosticSeverity;

impl ForgeApp {
    /// Per-frame LSP work: handle server messages and keep the active document synced.
    pub(super) fn update_language_servers(&mut self, ctx: &egui::Context) {
        for event in self.lsp.poll() {
            self.handle_lsp_event(event);
        }
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let config = self.settings.language_server(doc.extension());
        let root = self.workspace.as_ref().map(|ws| ws.root.path.clone());
        if let Some(error) = self.lsp.sync(doc, config.as_ref(), root.as_deref(), ctx) {
            self.status = format!("Error: {error}");
        }
    }

    fn handle_lsp_event(&mut self, event: LspEvent) {
        match event {
            LspEvent::Diagnostics { path, diagnostics } => {
                let Some(doc) = self
                    .documents
                    .iter_mut()
                    .find(|d| d.path.as_deref().map(normalize).as_ref() == Some(&path))
                else {
                    return;
                };
                let mut converted: Vec<Diagnostic> = diagnostics
                    .iter()
                    .map(|d| Diagnostic {
                        range: char_range(doc, &d.range),
                        severity: match d.severity {
                            Some(DiagnosticSeverity::WARNING) => Severity::Warning,
                            Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
                            Some(DiagnosticSeverity::HINT) => Severity::Hint,
                            _ => Severity::Error,
                        },
                        message: match &d.source {
                            Some(source) => format!("{} ({source})", d.message),
                            None => d.message.clone(),
                        },
                    })
                    .collect();
                converted.sort_by_key(|d| (d.range.start, d.severity));
                doc.diagnostics = converted;
            }
            LspEvent::Completion { path, items, .. } => {
                let Some(doc) = self.documents.get(self.active) else {
                    return;
                };
                if doc.path.as_deref().map(normalize).as_ref() != Some(&path) {
                    return;
                }
                if items.is_empty() {
                    self.status = "No completions".to_owned();
                    return;
                }
                let anchor = doc.buffer.word_start(self.cursor_offset);
                let mut popup = CompletionPopup::new(doc.id, anchor, items);
                popup.set_prefix(prefix(doc, anchor, self.cursor_offset));
                self.completion = (!popup.is_empty()).then_some(popup);
            }
            LspEvent::Definition(locations) => {
                let Some((path, range)) = locations.into_iter().next() else {
                    self.status = "No definition found".to_owned();
                    return;
                };
                self.open_file(&path);
                if let Some(doc) = self.documents.get(self.active)
                    && doc.path.as_deref().map(normalize).as_ref() == Some(&path)
                {
                    let at = char_range(doc, &range).start;
                    self.select(at..at, true);
                }
            }
            LspEvent::Status(message) => self.status = message,
        }
    }

    pub(super) fn trigger_completion(&mut self) {
        if let Some(doc) = self.documents.get(self.active)
            && !self.lsp.request_completion(doc, self.cursor_offset)
        {
            self.status = "No language server for this file".to_owned();
        }
    }

    pub(super) fn go_to_definition(&mut self) {
        if let Some(doc) = self.documents.get(self.active)
            && !self.lsp.request_definition(doc, self.cursor_offset)
        {
            self.status = "No language server for this file".to_owned();
        }
    }

    /// Selects the next (or previous) diagnostic after the cursor, wrapping around.
    pub(super) fn goto_problem(&mut self, forward: bool) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let diags = &doc.diagnostics;
        if diags.is_empty() {
            self.status = "No problems".to_owned();
            return;
        }
        let at = self.cursor_offset;
        let idx = if forward {
            diags.iter().position(|d| d.range.start > at).unwrap_or(0)
        } else {
            diags
                .iter()
                .rposition(|d| d.range.start < at)
                .unwrap_or(diags.len() - 1)
        };
        let d = &diags[idx];
        self.status = d.message.lines().next().unwrap_or_default().to_owned();
        let range = d.range.clone();
        self.select(range, true);
    }

    /// Keeps the completion popup in step with typing and auto-triggers completion
    /// after the server's trigger characters. Runs after the editor has rendered.
    pub(super) fn after_editor_update(&mut self, revision_before: u64) {
        let Some(doc) = self.documents.get(self.active) else {
            self.completion = None;
            return;
        };
        let cursor = self.cursor_offset;
        if let Some(popup) = &mut self.completion {
            let typed =
                (popup.doc == doc.id && cursor >= popup.anchor && self.selection.is_empty())
                    .then(|| prefix(doc, popup.anchor, cursor));
            match typed {
                Some(p) if p.chars().all(is_word_char) => {
                    popup.set_prefix(p);
                    if popup.is_empty() {
                        self.completion = None;
                    }
                }
                _ => self.completion = None,
            }
        }
        if doc.buffer.revision() != revision_before && self.selection.is_empty() {
            let before =
                &doc.buffer.as_str()[..doc.buffer.byte_index_from_char_index(cursor.into()).0];
            if self
                .lsp
                .trigger_characters(doc)
                .iter()
                .any(|t| !t.is_empty() && before.ends_with(t.as_str()))
            {
                self.trigger_completion();
            }
        }
    }

    /// Navigation keys for the completion popup; must run before the editor.
    pub(super) fn completion_keys(&mut self, ctx: &egui::Context) {
        let Some(popup) = &mut self.completion else {
            return;
        };
        match popup.handle_keys(ctx) {
            Some(CompletionKey::Accept) => self.accept_completion(),
            Some(CompletionKey::Dismiss) => self.completion = None,
            None => {}
        }
    }

    fn accept_completion(&mut self) {
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(item) = popup.selected_item().cloned() else {
            return;
        };
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let selection = completion::apply(doc, &item, popup.anchor, self.cursor_offset);
        self.select(selection, true);
    }

    pub(super) fn completion_popup(&mut self, ctx: &egui::Context) {
        if let (Some(popup), Some(pos)) = (&mut self.completion, self.cursor_screen_pos)
            && popup.ui(ctx, pos)
        {
            self.accept_completion();
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prefix(doc: &crate::editor::Document, anchor: usize, cursor: usize) -> &str {
    let anchor = anchor.min(cursor);
    doc.buffer.char_range(anchor.into()..cursor.into())
}

/// Underlines diagnostics in the visible part of the editor. Returns the message
/// of the diagnostic under the pointer, if any.
pub(super) fn paint_diagnostics(
    ui: &egui::Ui,
    output: &TextEditOutput,
    diagnostics: &[Diagnostic],
) -> Option<String> {
    if diagnostics.is_empty() {
        return None;
    }
    let galley = &output.galley;
    let origin = output.galley_pos.to_vec2();
    let clip = ui.clip_rect();
    let first = galley.cursor_from_pos(clip.min - output.galley_pos).index.0;
    let last = galley.cursor_from_pos(clip.max - output.galley_pos).index.0;
    let hover = ui.input(|i| i.pointer.hover_pos());
    let visuals = ui.visuals();
    let mut hovered: Vec<&str> = Vec::new();

    // Paint the least severe first so errors end up on top.
    let mut visible: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.range.end >= first && d.range.start <= last)
        .collect();
    visible.sort_by_key(|d| std::cmp::Reverse(d.severity));

    for d in visible {
        let a = galley
            .pos_from_cursor(CCursor::new(d.range.start))
            .translate(origin);
        let b = galley
            .pos_from_cursor(CCursor::new(d.range.end))
            .translate(origin);
        let same_row = (a.top() - b.top()).abs() < 1.0;
        let right = if same_row {
            b.left()
        } else {
            // Underline to the end of the first row.
            galley
                .rows
                .iter()
                .map(|r| r.rect().translate(origin))
                .find(|r| r.top() <= a.center().y && a.center().y <= r.bottom())
                .map_or(a.left() + 8.0, |r| r.right())
        };
        let right = right.max(a.left() + 6.0); // zero-width diagnostics still show
        let color = match d.severity {
            Severity::Error => visuals.error_fg_color,
            Severity::Warning => visuals.warn_fg_color,
            Severity::Info => egui::Color32::from_rgb(80, 150, 230),
            Severity::Hint => visuals.weak_text_color(),
        };
        let y = a.bottom() - 1.0;
        if d.severity == Severity::Hint {
            ui.painter().line_segment(
                [egui::pos2(a.left(), y), egui::pos2(right, y)],
                egui::Stroke::new(1.0, color),
            );
        } else {
            ui.painter().add(egui::Shape::line(
                squiggle(a.left(), right, y),
                egui::Stroke::new(1.0, color),
            ));
        }
        let area = egui::Rect::from_min_max(a.left_top(), egui::pos2(right, a.bottom()));
        if hover.is_some_and(|p| area.contains(p)) {
            hovered.push(&d.message);
        }
    }
    (!hovered.is_empty()).then(|| hovered.join("\n\n"))
}

fn squiggle(left: f32, right: f32, y: f32) -> Vec<egui::Pos2> {
    const STEP: f32 = 2.0;
    let mut points = Vec::new();
    let mut x = left;
    let mut up = false;
    while x <= right {
        points.push(egui::pos2(x, if up { y - 1.5 } else { y + 0.5 }));
        x += STEP;
        up = !up;
    }
    points
}

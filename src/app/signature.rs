//! Signature help: while typing a call's arguments, the function's
//! signature above the caret with the current parameter highlighted.

use std::path::{Path, PathBuf};

use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Key, Modifiers, Rect, RichText, TextStyle, vec2};
use lsp_types::{Documentation, SignatureHelp};

use super::ForgeApp;
use crate::lsp::Feature;
use crate::lsp::features::parameter_range;
use crate::lsp::text::Encoding;
use crate::theme::UiColors;
use crate::ui::markdown;

pub(super) struct SignaturePopup {
    doc: u64,
    help: SignatureHelp,
    encoding: Encoding,
    /// The signature shown, when there are overloads.
    active: usize,
    /// The caret when it was last asked for, to follow it as it moves.
    caret: usize,
}

impl ForgeApp {
    /// Cmd/Ctrl+Shift+Space: shows the signature of the call at the caret.
    pub(super) fn trigger_parameter_hints(&mut self, ctx: &egui::Context) {
        let Some((path, at)) = self.active_file_caret(ctx) else {
            return;
        };
        let text = self.documents[self.active].text().to_owned();
        let open = self.language.signature.is_some();
        if let Err(e) = self.lsp.signature_help(&path, &text, at, None, open) {
            self.status = e;
        }
    }

    fn active_file_caret(&self, ctx: &egui::Context) -> Option<(PathBuf, usize)> {
        let doc = self.documents.get(self.active)?;
        Some((doc.path.clone()?, self.selection(ctx).end))
    }

    /// Opens signature help on trigger characters such as `(`, and keeps it
    /// up to date (or closes it) as the caret moves while it is open.
    pub(super) fn update_signature(
        &mut self,
        doc_id: u64,
        path: &Path,
        caret: Option<usize>,
        typed: Option<char>,
    ) {
        let Some(caret) = caret else {
            self.language.signature = None;
            return;
        };
        if self
            .language
            .signature
            .as_ref()
            .is_some_and(|s| s.doc != doc_id)
        {
            self.language.signature = None;
        }
        if !self.lsp.supports(path, Feature::SignatureHelp) {
            return;
        }
        let open = self.language.signature.is_some();
        let (triggers, retriggers) = self.lsp.signature_triggers(path);
        let request = match typed {
            Some(c) if triggers.contains(&c) => Some(Some(c)),
            Some(c) if open && retriggers.contains(&c) => Some(Some(c)),
            Some(_) if open => Some(None),
            _ if open
                && self
                    .language
                    .signature
                    .as_ref()
                    .is_some_and(|s| s.caret != caret) =>
            {
                Some(None)
            }
            _ => None,
        };
        let Some(trigger) = request else {
            return;
        };
        if let Some(s) = &mut self.language.signature {
            s.caret = caret;
        }
        let text = self.documents[self.active].text().to_owned();
        let _ = self.lsp.signature_help(path, &text, caret, trigger, open);
    }

    /// Asks for signature help after a completion put the caret inside a
    /// call's parentheses, as accepting `foo($1)` does.
    pub(super) fn signature_after_completion(&mut self, ctx: &egui::Context) {
        let Some((path, at)) = self.active_file_caret(ctx) else {
            return;
        };
        let text = self.documents[self.active].text();
        let before = text.chars().nth(at.wrapping_sub(1));
        if before == Some('(') && self.lsp.supports(&path, Feature::SignatureHelp) {
            let text = text.to_owned();
            let _ = self.lsp.signature_help(&path, &text, at, Some('('), false);
        }
    }

    pub(super) fn on_signature_help(
        &mut self,
        path: &Path,
        at: usize,
        help: Option<SignatureHelp>,
        encoding: Encoding,
    ) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if doc.path.as_deref() != Some(path) {
            return;
        }
        let Some(help) = help else {
            self.language.signature = None;
            return;
        };
        // Keep an overload the user picked, unless the server moved on.
        let active = match &self.language.signature {
            Some(s) if s.help.signatures.len() == help.signatures.len() => s.active,
            _ => help.active_signature.unwrap_or(0) as usize,
        };
        let active = active.min(help.signatures.len() - 1);
        self.language.signature = Some(SignaturePopup {
            doc: doc.id,
            help,
            encoding,
            active,
            caret: at,
        });
    }

    /// Escape closes signature help (when no other popup wants it).
    pub(super) fn signature_keys(&mut self, ctx: &egui::Context) {
        if self.language.signature.is_none() || self.language.completion_open() {
            return;
        }
        if self.active_pane.is_some() || !self.editor_has_focus(ctx) {
            self.language.signature = None;
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.language.signature = None;
        }
    }

    pub(super) fn show_signature(&mut self, ctx: &egui::Context, caret: Rect, colors: &UiColors) {
        let Some(popup) = &mut self.language.signature else {
            return;
        };
        let count = popup.help.signatures.len();
        let signature = &popup.help.signatures[popup.active];
        let param = signature
            .active_parameter
            .or(popup.help.active_parameter)
            .and_then(|i| signature.parameters.as_ref()?.get(i as usize));
        let highlight =
            param.and_then(|p| parameter_range(&signature.label, &p.label, popup.encoding));
        let mut step = 0isize;
        egui::Area::new(egui::Id::unique("lsp_signature"))
            .order(egui::Order::Foreground)
            .fixed_pos(caret.left_top() - vec2(0.0, 4.0))
            .pivot(Align2::LEFT_BOTTOM)
            .constrain(true)
            .interactable(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(620.0);
                    ui.horizontal(|ui| {
                        if count > 1 {
                            if ui.small_button("‹").clicked() {
                                step = -1;
                            }
                            ui.weak(RichText::new(format!("{}/{count}", popup.active + 1)).small());
                            if ui.small_button("›").clicked() {
                                step = 1;
                            }
                        }
                        let font = TextStyle::Monospace.resolve(ui.style());
                        let text = ui.visuals().text_color();
                        let mut job = LayoutJob::default();
                        let label = &signature.label;
                        let r = highlight.clone().unwrap_or(0..0);
                        job.append(
                            &label[..r.start],
                            0.0,
                            TextFormat::simple(font.clone(), text),
                        );
                        let mut strong = TextFormat::simple(font.clone(), colors.accent.0);
                        strong.underline = egui::Stroke::new(1.0, colors.accent.0);
                        job.append(&label[r.clone()], 0.0, strong);
                        job.append(&label[r.end..], 0.0, TextFormat::simple(font, text));
                        job.wrap.max_width = 600.0;
                        ui.label(job);
                    });
                    let param_docs = param.and_then(|p| docs_text(p.documentation.as_ref()));
                    let docs = docs_text(signature.documentation.as_ref());
                    if param_docs.is_some() || docs.is_some() {
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .auto_shrink([true, true])
                            .show(ui, |ui| {
                                if let Some(d) = &param_docs {
                                    markdown::show(ui, d);
                                }
                                if let Some(d) = &docs {
                                    if param_docs.is_some() {
                                        ui.add_space(4.0);
                                    }
                                    markdown::show(ui, d);
                                }
                            });
                    }
                });
            });
        if step != 0 {
            popup.active = (popup.active as isize + step).rem_euclid(count as isize) as usize;
        }
    }
}

fn docs_text(docs: Option<&Documentation>) -> Option<String> {
    let text = match docs? {
        Documentation::String(s) => s.clone(),
        Documentation::MarkupContent(m) => m.value.clone(),
    };
    (!text.trim().is_empty()).then_some(text)
}

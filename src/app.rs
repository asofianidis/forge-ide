//! Top-level application state and UI layout.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Key, KeyboardShortcut, Modifiers, TextStyle};

use crate::editor::{Document, Highlighter};
use crate::lsp::{ServerBrowser, ServerManager};
use crate::workspace::{FileKinds, FileNode, Workspace};

const SHORTCUT_NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SHORTCUT_OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SHORTCUT_SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SHORTCUT_CLOSE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);

pub struct ForgeApp {
    workspace: Option<Workspace>,
    documents: Vec<Document>,
    active: usize,
    highlighter: Highlighter,
    status: String,
    /// 1-based (line, column) of the cursor in the active document.
    cursor: (usize, usize),
    servers: ServerManager,
    server_browser: ServerBrowser,
    /// Languages whose "install a language server" hint the user dismissed.
    dismissed_hints: HashSet<&'static str>,
}

impl ForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, root: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Self {
            workspace: root.filter(|p| p.is_dir()).map(Workspace::open),
            documents: vec![Document::untitled()],
            active: 0,
            highlighter: Highlighter::default(),
            status: "Ready".to_owned(),
            cursor: (1, 1),
            servers: ServerManager::new(cc.egui_ctx.clone()),
            server_browser: ServerBrowser::default(),
            dismissed_hints: HashSet::new(),
        }
    }

    // ---- actions -------------------------------------------------------

    fn new_file(&mut self) {
        self.documents.push(Document::untitled());
        self.active = self.documents.len() - 1;
    }

    fn open_file(&mut self, path: &Path) {
        if let Some(idx) = self
            .documents
            .iter()
            .position(|d| d.path.as_deref() == Some(path))
        {
            self.active = idx;
            return;
        }
        match Document::open(path) {
            Ok(doc) => {
                self.documents.push(doc);
                self.active = self.documents.len() - 1;
                self.status = format!("Opened {}", path.display());
            }
            Err(e) => self.status = format!("Error: {e:#}"),
        }
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.open_file(&path);
        }
    }

    fn open_folder_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.status = format!("Opened folder {}", path.display());
            self.workspace = Some(Workspace::open(path));
        }
    }

    fn save_active(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let result = if doc.path.is_some() {
            doc.save()
        } else if let Some(path) = rfd::FileDialog::new().save_file() {
            doc.save_as(path)
        } else {
            return;
        };
        self.status = match result {
            Ok(()) => format!("Saved {}", doc.title()),
            Err(e) => format!("Error: {e:#}"),
        };
    }

    fn close_tab(&mut self, idx: usize) {
        // TODO: prompt before discarding unsaved changes.
        if idx < self.documents.len() {
            self.documents.remove(idx);
        }
        if self.documents.is_empty() {
            self.documents.push(Document::untitled());
        }
        if self.active >= self.documents.len() || self.active > idx {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (new, open, save, close) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&SHORTCUT_NEW),
                i.consume_shortcut(&SHORTCUT_OPEN),
                i.consume_shortcut(&SHORTCUT_SAVE),
                i.consume_shortcut(&SHORTCUT_CLOSE),
            )
        });
        if new {
            self.new_file();
        }
        if open {
            self.open_file_dialog();
        }
        if save {
            self.save_active();
        }
        if close {
            self.close_tab(self.active);
        }
    }

    /// File kinds from the workspace scan plus any open tabs.
    fn project_file_kinds(&self) -> FileKinds {
        let mut kinds = self
            .workspace
            .as_ref()
            .map(|ws| ws.file_kinds.clone())
            .unwrap_or_default();
        for doc in self.documents.iter().filter(|d| d.path.is_some()) {
            kinds.insert_file(&doc.title());
        }
        kinds
    }

    // ---- panels --------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                let ctx = ui.ctx().clone();
                if ui
                    .add(
                        egui::Button::new("New File")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_NEW)),
                    )
                    .clicked()
                {
                    self.new_file();
                }
                if ui
                    .add(
                        egui::Button::new("Open File…")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_OPEN)),
                    )
                    .clicked()
                {
                    self.open_file_dialog();
                }
                if ui.button("Open Folder…").clicked() {
                    self.open_folder_dialog();
                }
                ui.separator();
                if ui
                    .add(
                        egui::Button::new("Save")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_SAVE)),
                    )
                    .clicked()
                {
                    self.save_active();
                }
                if ui
                    .add(
                        egui::Button::new("Close Tab")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_CLOSE)),
                    )
                    .clicked()
                {
                    self.close_tab(self.active);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Tools", |ui| {
                if ui.button("Language Servers…").clicked() {
                    self.server_browser.open = true;
                    ui.close();
                }
            });
        });
    }

    fn explorer(&mut self, ui: &mut egui::Ui) {
        ui.heading("Explorer");
        ui.separator();
        let Some(ws) = &mut self.workspace else {
            if ui.button("Open Folder…").clicked() {
                self.open_folder_dialog();
            }
            return;
        };
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| file_tree(ui, &mut ws.root, true, &mut clicked));
        if let Some(path) = clicked {
            self.open_file(&path);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.status);
            self.language_server_hint(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(doc) = self.documents.get(self.active) {
                    ui.label(doc.extension());
                    ui.separator();
                    ui.label(format!("Ln {}, Col {}", self.cursor.0, self.cursor.1));
                }
            });
        });
    }

    /// Offers to install a language server when the active file has none.
    fn language_server_hint(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.documents.get(self.active).filter(|d| d.path.is_some()) else {
            return;
        };
        let Some(spec) = self
            .servers
            .suggestions_for_file(&doc.title())
            .into_iter()
            .next()
        else {
            return;
        };
        let language = spec.languages[0];
        if self.dismissed_hints.contains(language) {
            return;
        }
        ui.separator();
        if ui
            .link(format!("💡 Get a {language} language server"))
            .on_hover_text("Browse language servers that add completion and diagnostics")
            .clicked()
        {
            self.server_browser.open_with_query(language);
        }
        if ui
            .small_button("×")
            .on_hover_text("Don't suggest again")
            .clicked()
        {
            self.dismissed_hints.insert(language);
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut to_close = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (idx, doc) in self.documents.iter().enumerate() {
                    let label = if doc.is_dirty() {
                        format!("● {}", doc.title())
                    } else {
                        doc.title()
                    };
                    if ui.selectable_label(idx == self.active, label).clicked() {
                        self.active = idx;
                    }
                    if ui.small_button("×").clicked() {
                        to_close = Some(idx);
                    }
                    ui.separator();
                }
            });
        });
        if let Some(idx) = to_close {
            self.close_tab(idx);
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let extension = doc.extension().to_owned();
        let highlighter = &mut self.highlighter;
        let font = TextStyle::Monospace.resolve(ui.style());

        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = highlighter.highlight(text.as_str(), &extension, font.clone());
            job.wrap.max_width = wrap_width;
            ui.painter().layout_job(job)
        };

        let output = egui::ScrollArea::both()
            .auto_shrink(false)
            .show(ui, |ui| {
                egui::TextEdit::multiline(&mut doc.text)
                    .id_salt(self.active)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(40)
                    .layouter(&mut layouter)
                    .show(ui)
            })
            .inner;

        if let Some(range) = output.cursor_range {
            self.cursor = doc.line_col(range.primary.index.into());
        }
    }
}

/// Recursively renders a directory tree. Sets `clicked` when a file is selected.
fn file_tree(ui: &mut egui::Ui, node: &mut FileNode, is_root: bool, clicked: &mut Option<PathBuf>) {
    if node.is_dir {
        let header = egui::CollapsingHeader::new(&node.name)
            .id_salt(&node.path)
            .default_open(is_root)
            .show(ui, |ui| {
                for child in node.children() {
                    file_tree(ui, child, false, clicked);
                }
            });
        header.header_response.context_menu(|ui| {
            if ui.button("Refresh").clicked() {
                node.refresh();
                ui.close();
            }
        });
    } else if ui.selectable_label(false, &node.name).clicked() {
        *clicked = Some(node.path.clone());
    }
}

impl eframe::App for ForgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(&ui.ctx().clone());
        self.servers.poll();

        egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("explorer")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| self.explorer(ui));

        let bg = self.highlighter.background();
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).fill(bg))
            .show(ui, |ui| {
                self.tab_bar(ui);
                ui.separator();
                self.editor(ui);
            });

        if self.server_browser.open {
            let kinds = self.project_file_kinds();
            self.server_browser
                .show(&ui.ctx().clone(), &mut self.servers, &kinds);
        }
    }
}

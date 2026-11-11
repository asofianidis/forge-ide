//! Top-level application state and UI layout.

use std::path::{Path, PathBuf};

use egui::{Key, KeyboardShortcut, Modifiers, TextStyle};

use crate::editor::{Document, Highlighter, gutter};
use crate::settings::Settings;
use crate::theme::{self, Theme};
use crate::updater::{self, UpdateState, Updater};
use crate::workspace::{FileNode, Workspace};

const SHORTCUT_NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SHORTCUT_OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SHORTCUT_SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SHORTCUT_CLOSE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);

pub struct ForgeApp {
    workspace: Option<Workspace>,
    documents: Vec<Document>,
    active: usize,
    highlighter: Highlighter,
    /// Built-in and custom themes; `theme` indexes into this.
    themes: Vec<Theme>,
    theme: usize,
    status: String,
    /// 1-based (line, column) of the cursor in the active document.
    cursor: (usize, usize),
    settings: Settings,
    updater: Updater,
    /// Update state seen last frame, to report transitions in the status bar.
    last_update_state: UpdateState,
    /// The current check was started from the menu, so "up to date" and
    /// errors are worth reporting.
    manual_update_check: bool,
}

impl ForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, root: Option<PathBuf>) -> Self {
        let settings = Settings::load();
        let (themes, errors) = theme::load_all(theme::themes_dir().as_deref());
        let wanted = settings
            .theme
            .clone()
            .unwrap_or_else(|| theme::DEFAULT_THEME.to_owned());
        let idx = themes.iter().position(|t| t.name == wanted).unwrap_or(0);

        let updater = Updater::default();
        if settings.auto_update.unwrap_or(updater::IS_RELEASE_BUILD) {
            updater.check(&cc.egui_ctx);
        }

        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 4.0);
            style.spacing.menu_margin = egui::Margin::same(8);
        });
        themes[idx].apply(&cc.egui_ctx);

        Self {
            workspace: root.filter(|p| p.is_dir()).map(Workspace::open),
            documents: vec![Document::untitled()],
            active: 0,
            highlighter: Highlighter::new(themes[idx].syntect_theme()),
            theme: idx,
            themes,
            status: theme_errors_status(&errors).unwrap_or_else(|| "Ready".to_owned()),
            cursor: (1, 1),
            settings,
            updater,
            last_update_state: UpdateState::Idle,
            manual_update_check: false,
        }
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.status = format!("Could not save settings: {e:#}");
        }
    }

    // ---- updates -------------------------------------------------------

    fn check_for_updates(&mut self, ctx: &egui::Context) {
        self.manual_update_check = true;
        self.status = "Checking for updates…".to_owned();
        self.updater.check(ctx);
    }

    fn has_unsaved_changes(&self) -> bool {
        self.documents.iter().any(Document::is_dirty)
    }

    /// Reports update state changes in the status bar.
    fn poll_updater(&mut self) {
        let state = self.updater.state();
        if state == self.last_update_state {
            return;
        }
        match &state {
            UpdateState::UpToDate if self.manual_update_check => {
                self.status = format!("Forge IDE {} is up to date", updater::VERSION);
            }
            UpdateState::Available(v) => {
                self.status = format!("Forge IDE {v} is available");
            }
            UpdateState::Installed(v) => {
                self.status = format!("Forge IDE {v} installed. Restart to use it.");
            }
            // Failed background checks (e.g. offline) stay quiet; failed
            // installs and manual checks are reported.
            UpdateState::Failed(msg)
                if self.manual_update_check
                    || matches!(self.last_update_state, UpdateState::Installing(_)) =>
            {
                self.status = msg.clone();
            }
            _ => {}
        }
        if !matches!(state, UpdateState::Checking) {
            self.manual_update_check = false;
        }
        self.last_update_state = state;
    }

    /// Update indicator on the right side of the status bar.
    fn update_indicator(&mut self, ui: &mut egui::Ui) {
        let accent = self.current_theme().ui.accent.0;
        let accent_text = self.current_theme().ui.accent_text.0;
        let accent_button = |text: String| {
            egui::Button::new(egui::RichText::new(text).color(accent_text))
                .fill(accent)
                .small()
        };
        match self.updater.state() {
            UpdateState::Available(v) => {
                if ui
                    .add(accent_button(format!("Update to {v}")))
                    .on_hover_text(format!("You have {}", updater::VERSION))
                    .clicked()
                {
                    self.updater.install(ui.ctx());
                }
                ui.separator();
            }
            UpdateState::Installing(progress) => {
                let text = match progress {
                    Some(p) => format!("Updating… {:.0}%", p * 100.0),
                    None => "Updating…".to_owned(),
                };
                ui.label(text);
                ui.spinner();
                ui.separator();
            }
            UpdateState::Installed(_) => {
                let unsaved = self.has_unsaved_changes();
                let response = ui
                    .add_enabled(!unsaved, accent_button("Restart to update".to_owned()))
                    .on_disabled_hover_text("Save your changes first");
                if response.clicked()
                    && let Err(e) = updater::restart()
                {
                    self.status = format!("Could not restart: {e:#}");
                }
                ui.separator();
            }
            _ => {}
        }
    }

    fn current_theme(&self) -> &Theme {
        &self.themes[self.theme]
    }

    // ---- themes --------------------------------------------------------

    fn set_theme(&mut self, ctx: &egui::Context, idx: usize) {
        self.theme = idx;
        let theme = &self.themes[idx];
        theme.apply(ctx);
        self.highlighter.set_theme(theme.syntect_theme());
        self.status = format!("Theme: {}", theme.name);
        self.settings.theme = Some(theme.name.clone());
        self.save_settings();
    }

    fn reload_themes(&mut self, ctx: &egui::Context) {
        let name = self.current_theme().name.clone();
        let (themes, errors) = theme::load_all(theme::themes_dir().as_deref());
        self.themes = themes;
        self.theme = self.themes.iter().position(|t| t.name == name).unwrap_or(0);
        let theme = &self.themes[self.theme];
        theme.apply(ctx);
        self.highlighter.set_theme(theme.syntect_theme());
        self.status = theme_errors_status(&errors)
            .unwrap_or_else(|| format!("Loaded {} themes", self.themes.len()));
    }

    fn open_themes_folder(&mut self) {
        let Some(dir) = theme::themes_dir() else {
            self.status = "Error: no config directory".to_owned();
            return;
        };
        let result = std::fs::create_dir_all(&dir)
            .and_then(|()| {
                // Seed an empty folder with the documented example.
                if std::fs::read_dir(&dir)?.next().is_none() {
                    std::fs::write(
                        dir.join("example.toml"),
                        include_str!("../themes/example.toml"),
                    )?;
                }
                Ok(())
            })
            .map_err(anyhow::Error::from)
            .and_then(|()| open_path(&dir));
        self.status = match result {
            Ok(()) => format!("Themes folder: {}", dir.display()),
            Err(e) => format!("Error: {e:#}"),
        };
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
            ui.menu_button("View", |ui| {
                ui.menu_button("Theme", |ui| {
                    let mut chosen = None;
                    for (idx, theme) in self.themes.iter().enumerate() {
                        if ui.radio(idx == self.theme, &theme.name).clicked() {
                            chosen = Some(idx);
                        }
                    }
                    if let Some(idx) = chosen {
                        self.set_theme(ui.ctx(), idx);
                    }
                    ui.separator();
                    if ui.button("Reload Themes").clicked() {
                        self.reload_themes(ui.ctx());
                    }
                    if ui.button("Open Themes Folder").clicked() {
                        self.open_themes_folder();
                    }
                });
            });
            ui.menu_button("Help", |ui| {
                let busy = matches!(
                    self.updater.state(),
                    UpdateState::Checking | UpdateState::Installing(_)
                );
                if ui
                    .add_enabled(!busy, egui::Button::new("Check for Updates"))
                    .clicked()
                {
                    self.check_for_updates(ui.ctx());
                }
                let mut auto = self
                    .settings
                    .auto_update
                    .unwrap_or(updater::IS_RELEASE_BUILD);
                if ui
                    .checkbox(&mut auto, "Check for Updates on Startup")
                    .changed()
                {
                    self.settings.auto_update = Some(auto);
                    self.save_settings();
                }
                ui.separator();
                ui.label(format!("Forge IDE {}", updater::VERSION));
            });
        });
    }

    fn explorer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("EXPLORER")
                .small()
                .strong()
                .color(self.current_theme().ui.text_muted.0),
        );
        ui.add_space(2.0);
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.update_indicator(ui);
                if let Some(doc) = self.documents.get(self.active) {
                    ui.label(doc.extension());
                    ui.separator();
                    ui.label(format!("Ln {}, Col {}", self.cursor.0, self.cursor.1));
                }
            });
        });
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut to_close = None;
        let colors = self.current_theme().ui.clone();
        egui::ScrollArea::horizontal()
            .id_salt("tab_bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (idx, doc) in self.documents.iter().enumerate() {
                        let selected = idx == self.active;
                        let label = if doc.is_dirty() {
                            format!("● {}", doc.title())
                        } else {
                            doc.title()
                        };
                        let text = egui::RichText::new(label).color(if selected {
                            colors.text.0
                        } else {
                            colors.text_muted.0
                        });
                        let fill = if selected {
                            colors.widget.0
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let response = ui.add(
                            egui::Button::new(text)
                                .fill(fill)
                                .stroke(egui::Stroke::NONE),
                        );
                        if selected {
                            // Accent underline marks the active tab.
                            let r = response.rect;
                            ui.painter().hline(
                                r.x_range().shrink(6.0),
                                r.bottom() + 1.0,
                                egui::Stroke::new(2.0, colors.accent.0),
                            );
                        }
                        if response.clicked() {
                            self.active = idx;
                        }
                        if ui
                            .add(egui::Button::new("×").small().frame(false))
                            .on_hover_text("Close")
                            .clicked()
                        {
                            to_close = Some(idx);
                        }
                        ui.add_space(4.0);
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
        let digit_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let gutter_width = gutter::width(doc.line_count(), digit_width);
        let current_line = self.cursor.0;

        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = highlighter.highlight(text.as_str(), &extension, font.clone());
            job.wrap.max_width = wrap_width;
            ui.painter().layout_job(job)
        };

        let output = egui::ScrollArea::both()
            .id_salt("editor_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                let output = egui::TextEdit::multiline(&mut doc.text)
                    .id_salt(self.active)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(40)
                    .margin(egui::Margin {
                        left: gutter_width as i8,
                        right: 4,
                        top: 2,
                        bottom: 2,
                    })
                    .layouter(&mut layouter)
                    .show(ui);
                gutter::paint(ui, &output, current_line, &font);
                output
            })
            .inner;

        if let Some(range) = output.cursor_range {
            self.cursor = doc.line_col(range.primary.index.into());
        }
    }
}

/// Formats theme loading errors for the status bar.
fn theme_errors_status(errors: &[String]) -> Option<String> {
    match errors {
        [] => None,
        [one] => Some(format!("Theme error: {one}")),
        [first, rest @ ..] => Some(format!("Theme error: {first} (and {} more)", rest.len())),
    }
}

/// Opens a file or folder with the platform's default handler.
fn open_path(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program).arg(path).spawn()?;
    Ok(())
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
        self.poll_updater();

        let panel_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 6));
        egui::Panel::top("menu_bar")
            .frame(panel_frame)
            .show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status_bar")
            .frame(panel_frame)
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("explorer")
            .frame(panel_frame.inner_margin(egui::Margin::same(10)))
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
    }
}

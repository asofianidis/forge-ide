//! Top-level application state and UI layout.

mod editing;
mod find_bar;
mod pickers;
mod search_panel;

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use egui::text::CCursor;
use egui::{Galley, Key, Modifiers, Rect, RichText, Shape, Stroke, TextStyle, pos2};

use crate::commands::Command;
use crate::editor::find::Match;
use crate::editor::ops;
use crate::editor::{Document, Highlighter, gutter};
use crate::lsp::{ServerBrowser, ServerManager};
use crate::palette::Picker;
use crate::search::FileIndex;
use crate::settings::Settings;
use crate::theme::{self, Theme};
use crate::updater::{self, UpdateState, Updater};
use crate::workspace::{FileKinds, FileNode, Workspace};

use editing::editor_id;
use find_bar::FindBar;
use pickers::{FileMatches, PickerMode};
use search_panel::SearchPanel;

/// What the left sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Sidebar {
    #[default]
    Explorer,
    Search,
}

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
    servers: ServerManager,
    server_browser: ServerBrowser,
    /// Languages whose "install a language server" hint the user dismissed.
    dismissed_hints: HashSet<&'static str>,
    settings: Settings,
    updater: Updater,
    /// Update state seen last frame, to report transitions in the status bar.
    last_update_state: UpdateState,
    /// The current check was started from the menu, so "up to date" and
    /// errors are worth reporting.
    manual_update_check: bool,
    /// Commands chosen from menus or the palette, run at the end of the frame.
    pending_commands: Vec<Command>,
    sidebar: Sidebar,
    find: FindBar,
    search: SearchPanel,
    picker: Picker,
    picker_mode: PickerMode,
    file_index: FileIndex,
    /// Bumped whenever `file_index` gets a new file list.
    file_index_generation: usize,
    file_matches: FileMatches,
    /// Input of the open Go to Line dialog.
    go_to_line: Option<String>,
    /// Character index to scroll the editor to on the next frame.
    scroll_to: Option<usize>,
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
            servers: ServerManager::new(cc.egui_ctx.clone()),
            server_browser: ServerBrowser::default(),
            dismissed_hints: HashSet::new(),
            settings,
            updater,
            last_update_state: UpdateState::Idle,
            manual_update_check: false,
            pending_commands: Vec::new(),
            sidebar: Sidebar::default(),
            find: FindBar::default(),
            search: SearchPanel::default(),
            picker: Picker::default(),
            picker_mode: PickerMode::default(),
            file_index: FileIndex::default(),
            file_index_generation: 0,
            file_matches: FileMatches::default(),
            go_to_line: None,
            scroll_to: None,
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
            self.search = SearchPanel::default();
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
        if self.picker.open || self.go_to_line.is_some() {
            // Let the dialog handle Escape; other shortcuts still work.
        } else if self.find.open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.close_find(ctx);
        }
        self.handle_tab(ctx);

        // Editing shortcuts only apply when typing in the editor, so that
        // e.g. Alt+Up in the find field doesn't move lines.
        let editor_focused = self.editor_has_focus(ctx)
            || (ctx.memory(|m| m.focused().is_none())
                && !self.picker.open
                && self.go_to_line.is_none());
        for (cmd, shortcut) in Command::shortcut_table() {
            if cmd.edits_text() && !editor_focused {
                continue;
            }
            if ctx.input_mut(|i| i.consume_shortcut(&shortcut)) {
                self.run_command(ctx, cmd);
            }
        }
    }

    fn run_command(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::NewFile => self.new_file(),
            Command::OpenFile => self.open_file_dialog(),
            Command::OpenFolder => self.open_folder_dialog(),
            Command::Save => self.save_active(),
            Command::CloseTab => self.close_tab(self.active),
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::Find => self.open_find(ctx, false),
            Command::Replace => self.open_find(ctx, true),
            Command::FindInProject => self.open_project_search(ctx),
            Command::ToggleComment
            | Command::Indent
            | Command::Outdent
            | Command::DuplicateLine
            | Command::MoveLineUp
            | Command::MoveLineDown => self.edit_command(ctx, cmd),
            Command::QuickOpen => self.open_quick_open(ctx),
            Command::GoToLine => self.open_go_to_line(),
            Command::ShowCommandPalette => self.open_command_palette(),
            Command::ShowExplorer => self.sidebar = Sidebar::Explorer,
            Command::SelectTheme => self.open_theme_picker(),
            Command::ReloadThemes => self.reload_themes(ctx),
            Command::OpenThemesFolder => self.open_themes_folder(),
            Command::LanguageServers => self.server_browser.open = true,
            Command::CheckForUpdates => self.check_for_updates(ctx),
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

    /// A menu entry for `cmd`, showing its shortcut. Clicking queues the command.
    fn menu_item(&mut self, ui: &mut egui::Ui, cmd: Command) {
        let mut button = egui::Button::new(cmd.label());
        if let Some(shortcut) = cmd.shortcuts().first() {
            button = button.shortcut_text(ui.ctx().format_shortcut(shortcut));
        }
        let enabled = match cmd {
            Command::CheckForUpdates => !matches!(
                self.updater.state(),
                UpdateState::Checking | UpdateState::Installing(_)
            ),
            Command::QuickOpen | Command::FindInProject => self.workspace.is_some(),
            _ => true,
        };
        if ui.add_enabled(enabled, button).clicked() {
            self.pending_commands.push(cmd);
            ui.close();
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        use Command::*;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                for cmd in [NewFile, OpenFile, OpenFolder] {
                    self.menu_item(ui, cmd);
                }
                ui.separator();
                self.menu_item(ui, Save);
                self.menu_item(ui, CloseTab);
                ui.separator();
                self.menu_item(ui, Quit);
            });
            ui.menu_button("Edit", |ui| {
                for cmd in [Find, Replace, FindInProject] {
                    self.menu_item(ui, cmd);
                }
                ui.separator();
                for cmd in [ToggleComment, Indent, Outdent] {
                    self.menu_item(ui, cmd);
                }
                ui.separator();
                for cmd in [DuplicateLine, MoveLineUp, MoveLineDown] {
                    self.menu_item(ui, cmd);
                }
            });
            ui.menu_button("Go", |ui| {
                self.menu_item(ui, QuickOpen);
                self.menu_item(ui, GoToLine);
            });
            ui.menu_button("View", |ui| {
                self.menu_item(ui, ShowCommandPalette);
                ui.separator();
                self.menu_item(ui, ShowExplorer);
                let mut search = egui::Button::new("Search");
                if let Some(s) = FindInProject.shortcuts().first() {
                    search = search.shortcut_text(ui.ctx().format_shortcut(s));
                }
                if ui.add(search).clicked() {
                    self.pending_commands.push(FindInProject);
                    ui.close();
                }
                ui.separator();
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
                    self.menu_item(ui, SelectTheme);
                    self.menu_item(ui, ReloadThemes);
                    self.menu_item(ui, OpenThemesFolder);
                });
            });
            ui.menu_button("Tools", |ui| {
                self.menu_item(ui, LanguageServers);
            });
            ui.menu_button("Help", |ui| {
                self.menu_item(ui, CheckForUpdates);
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

    /// Explorer / Search switcher at the top of the sidebar.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let muted = self.current_theme().ui.text_muted.0;
        let text = self.current_theme().ui.text.0;
        ui.horizontal(|ui| {
            for (view, label) in [(Sidebar::Explorer, "EXPLORER"), (Sidebar::Search, "SEARCH")] {
                let selected = self.sidebar == view;
                let color = if selected { text } else { muted };
                let r = ui.add(
                    egui::Button::new(RichText::new(label).small().strong().color(color))
                        .frame(false),
                );
                if r.clicked() {
                    if view == Sidebar::Search {
                        self.pending_commands.push(Command::FindInProject);
                    } else {
                        self.sidebar = view;
                    }
                }
            }
        });
        ui.add_space(2.0);
        match self.sidebar {
            Sidebar::Explorer => self.explorer(ui),
            Sidebar::Search => self.search_view(ui),
        }
    }

    fn explorer(&mut self, ui: &mut egui::Ui) {
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
                self.update_indicator(ui);
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
        let id = editor_id(doc);
        let extension = doc.extension().to_owned();
        let highlighter = &mut self.highlighter;
        let font = TextStyle::Monospace.resolve(ui.style());
        let digit_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let gutter_width = gutter::width(doc.line_count(), digit_width);
        let current_line = self.cursor.0;
        let scroll_to = self.scroll_to.take();
        let matches: &[Match] = if self.find.open {
            &self.find.matches
        } else {
            &[]
        };
        let current_match = self.find.current;

        let visuals = ui.visuals();
        let background = visuals.text_edit_bg_color();
        let accent = self.themes[self.theme].ui.accent.0;
        let colors = HighlightColors {
            find: accent.gamma_multiply(0.25),
            find_current: accent.gamma_multiply(0.55),
            bracket_fill: visuals.selection.bg_fill.gamma_multiply(0.35),
            bracket_stroke: Stroke::new(1.0, visuals.weak_text_color()),
        };

        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = highlighter.highlight(text.as_str(), &extension, font.clone());
            job.wrap.max_width = wrap_width;
            ui.painter().layout_job(job)
        };

        // The editor fills the whole central panel, so draw it without the default
        // `TextEdit` frame: that frame outlines the widget on hover and, since the
        // editor keeps focus, permanently in the selection stroke color. The
        // background is painted separately, underneath the match highlights.
        let frame = egui::Frame::NONE.inner_margin(egui::Margin {
            left: gutter_width as i8,
            right: 4,
            top: 2,
            bottom: 2,
        });

        let output = egui::ScrollArea::both()
            .id_salt("editor_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                let underlay = ui.painter().add(Shape::Noop);
                let output = egui::TextEdit::multiline(&mut doc.text)
                    .id(id)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(40)
                    .frame(frame)
                    .layouter(&mut layouter)
                    .show(ui);

                let galley = &output.galley;
                let offset = output.galley_pos.to_vec2();
                let mut shapes = vec![Shape::rect_filled(
                    output.response.response.rect,
                    0.0,
                    background,
                )];

                // Find matches, only those on screen.
                let visible = visible_chars(galley, ui.clip_rect().translate(-offset));
                let first = matches.partition_point(|m| m.chars.end < visible.start);
                for (i, m) in matches.iter().enumerate().skip(first) {
                    if m.chars.start > visible.end {
                        break;
                    }
                    let fill = if Some(i) == current_match {
                        colors.find_current
                    } else {
                        colors.find
                    };
                    for r in range_rects(galley, m.chars.clone()) {
                        shapes.push(Shape::rect_filled(r.translate(offset), 2.0, fill));
                    }
                }

                // Matching bracket pair around the caret.
                if let Some(range) = output.cursor_range
                    && range.is_empty()
                    && output.response.response.has_focus()
                    && let Some((a, b)) =
                        ops::matching_bracket(&doc.text, range.primary.index.into())
                {
                    for c in [a, b] {
                        let r = range_rects(galley, c..c + 1)[0].translate(offset);
                        shapes.push(Shape::rect_filled(r, 2.0, colors.bracket_fill));
                        shapes.push(Shape::rect_stroke(
                            r,
                            2.0,
                            colors.bracket_stroke,
                            egui::StrokeKind::Inside,
                        ));
                    }
                }
                ui.painter().set(underlay, Shape::Vec(shapes));

                gutter::paint(ui, &output, current_line, &font);
                if let Some(c) = scroll_to {
                    let r = galley.pos_from_cursor(CCursor::new(c)).translate(offset);
                    ui.scroll_to_rect(r.expand(digit_width * 4.0), Some(egui::Align::Center));
                }
                output
            })
            .inner;

        if let Some(range) = output.cursor_range {
            self.cursor = doc.line_col(range.primary.index.into());
        }
    }
}

struct HighlightColors {
    find: egui::Color32,
    find_current: egui::Color32,
    bracket_fill: egui::Color32,
    bracket_stroke: Stroke,
}

/// Range of character indices on rows that intersect `clip` (in galley coordinates).
fn visible_chars(galley: &Galley, clip: Rect) -> Range<usize> {
    let mut start = None;
    let mut chars = 0;
    for row in &galley.rows {
        let rect = row.rect();
        let count: usize = row.char_count_including_newline().into();
        if rect.bottom() >= clip.top() && start.is_none() {
            start = Some(chars);
        }
        if rect.top() > clip.bottom() {
            break;
        }
        chars += count;
    }
    start.unwrap_or(chars)..chars
}

/// Rectangles (in galley coordinates) covering the characters in `range`, one
/// per row the range spans.
fn range_rects(galley: &Galley, range: Range<usize>) -> Vec<Rect> {
    let a = galley.pos_from_cursor(CCursor::new(range.start));
    let b = galley.pos_from_cursor(CCursor::new(range.end));
    let min_width = 4.0;
    if (a.top() - b.top()).abs() < 0.5 {
        let right = b.left().max(a.left() + min_width);
        return vec![Rect::from_min_max(a.min, pos2(right, a.bottom()))];
    }
    // Spans rows: from the start to the end of its row, full rows in between,
    // and the start of the last row up to the end.
    let mut rects = Vec::new();
    for row in &galley.rows {
        let r = row.rect();
        if r.bottom() <= a.top() || r.top() >= b.bottom() {
            continue;
        }
        let left = if r.top() <= a.top() {
            a.left()
        } else {
            r.left()
        };
        let right = if r.bottom() >= b.bottom() {
            b.left()
        } else {
            r.right().max(left + min_width)
        };
        if right > left {
            rects.push(Rect::from_min_max(
                pos2(left, r.top()),
                pos2(right, r.bottom()),
            ));
        }
    }
    rects
}

/// A small on/off button for search options. Returns true when toggled.
fn toggle(ui: &mut egui::Ui, value: &mut bool, label: &str, hint: &str) -> bool {
    let clicked = ui
        .add(egui::Button::selectable(
            *value,
            RichText::new(label).monospace(),
        ))
        .on_hover_text(hint)
        .clicked();
    if clicked {
        *value = !*value;
    }
    clicked
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
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.servers.poll();
        self.poll_updater();
        self.search.search.poll();

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
            .show(ui, |ui| self.sidebar(ui));

        let bg = self.highlighter.background();
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).fill(bg))
            .show(ui, |ui| {
                self.tab_bar(ui);
                ui.separator();
                self.find_bar(ui);
                self.editor(ui);
            });

        if self.server_browser.open {
            let kinds = self.project_file_kinds();
            self.server_browser
                .show(&ui.ctx().clone(), &mut self.servers, &kinds);
        }

        self.show_picker(&ctx);
        self.go_to_line_dialog(&ctx);
        for cmd in std::mem::take(&mut self.pending_commands) {
            self.run_command(&ctx, cmd);
        }
    }
}

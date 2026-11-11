//! Top-level application state and UI layout.

mod language;

use std::ops::Range;
use std::path::{Path, PathBuf};

use egui::TextStyle;
use egui::text::{CCursor, CCursorRange};

use crate::commands::Command;
use crate::completion::CompletionPopup;
use crate::editor::{Document, Highlighter, Severity, gutter, search};
use crate::find_bar::{FindAction, FindBar, GotoLine};
use crate::lsp::LspManager;
use crate::palette::Palette;
use crate::settings::{self, Settings};
use crate::terminal::Terminal;
use crate::theme::{self, Theme};
use crate::updater::{self, UpdateState, Updater};
use crate::workspace::{FileNode, Workspace};

/// Something waiting on the user to decide what to do with unsaved changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseRequest {
    /// Close the tab holding the document with this id.
    Tab(egui::Id),
    /// Close the whole window.
    Window,
}

enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// A selection to apply to the active editor on the next frame.
struct PendingSelection {
    range: Range<usize>,
    /// Move keyboard focus to the editor (false while typing in the find bar).
    focus: bool,
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
    /// Current selection in the active document, as a char range (empty = caret).
    selection: Range<usize>,
    /// Char offset of the caret (the moving end of the selection).
    cursor_offset: usize,
    /// Screen position just below the caret, for anchoring popups.
    cursor_screen_pos: Option<egui::Pos2>,
    /// Selection to apply (and scroll to) in the active editor on the next frame.
    pending_selection: Option<PendingSelection>,
    find: FindBar,
    goto_line: GotoLine,
    palette: Palette,
    /// Close waiting on the unsaved-changes prompt.
    pending_close: Option<CloseRequest>,
    /// Set once the user has resolved unsaved changes, so the next close request goes through.
    allow_window_close: bool,
    settings: Settings,
    /// Where settings are persisted; `None` if the platform has no config dir.
    settings_path: Option<PathBuf>,
    settings_open: bool,
    /// The settings file was saved from the editor; reload it next frame.
    settings_reload_pending: bool,
    terminal: Option<Terminal>,
    terminal_open: bool,
    lsp: LspManager,
    completion: Option<CompletionPopup>,
    /// Give the terminal keyboard focus on the next frame.
    focus_terminal: bool,
    updater: Updater,
    /// Update state seen last frame, to report transitions in the status bar.
    last_update_state: UpdateState,
    /// The current check was started from the menu, so "up to date" and
    /// errors are worth reporting.
    manual_update_check: bool,
}

impl ForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, root: Option<PathBuf>) -> Self {
        // Ctrl+=/Ctrl+- change the editor font size instead of zooming the whole UI.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 4.0);
            style.spacing.menu_margin = egui::Margin::same(8);
        });

        let (themes, errors) = theme::load_all(theme::themes_dir().as_deref());
        let mut app = Self {
            workspace: root.filter(|p| p.is_dir()).map(Workspace::open),
            documents: vec![Document::untitled()],
            active: 0,
            highlighter: Highlighter::new(themes[0].syntect_theme()),
            theme: 0,
            themes,
            status: theme_errors_status(&errors).unwrap_or_else(|| "Ready".to_owned()),
            cursor: (1, 1),
            selection: 0..0,
            cursor_offset: 0,
            cursor_screen_pos: None,
            pending_selection: None,
            find: FindBar::default(),
            goto_line: GotoLine::default(),
            palette: Palette::default(),
            pending_close: None,
            allow_window_close: false,
            settings: Settings::default(),
            settings_path: Settings::default_path(),
            settings_open: false,
            settings_reload_pending: false,
            terminal: None,
            terminal_open: false,
            lsp: LspManager::default(),
            completion: None,
            focus_terminal: false,
            updater: Updater::default(),
            last_update_state: UpdateState::Idle,
            manual_update_check: false,
        };
        app.reload_settings(&cc.egui_ctx);
        if app
            .settings
            .auto_update
            .unwrap_or(updater::IS_RELEASE_BUILD)
        {
            app.updater.check(&cc.egui_ctx);
        }
        app
    }

    // ---- settings ------------------------------------------------------

    /// (Re)loads settings from disk and applies them.
    fn reload_settings(&mut self, ctx: &egui::Context) {
        if let Some(path) = &self.settings_path {
            match Settings::load(path) {
                Ok(s) => self.settings = s,
                Err(e) => self.status = format!("Error: {e:#}"),
            }
        }
        self.apply_settings(ctx);
    }

    /// Applies the current settings to the theme and egui style.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        match self
            .themes
            .iter()
            .position(|t| t.name == self.settings.theme)
        {
            Some(idx) => self.activate_theme(ctx, idx),
            None => {
                // Keep the unknown name in the settings so a custom theme that
                // failed to load isn't forgotten; fall back for this session.
                self.activate_theme(ctx, self.theme);
                self.status = format!(
                    "Unknown theme \"{}\", using {}",
                    self.settings.theme,
                    self.current_theme().name
                );
            }
        }
        let font_size = self.settings.font_size;
        ctx.all_styles_mut(|style| {
            if let Some(font) = style.text_styles.get_mut(&TextStyle::Monospace) {
                font.size = font_size;
            }
        });
    }

    fn save_settings(&mut self) {
        if let Some(path) = &self.settings_path
            && let Err(e) = self.settings.save(path)
        {
            self.status = format!("Could not save settings: {e:#}");
        }
    }

    /// Applies and persists changed settings.
    fn settings_changed(&mut self, ctx: &egui::Context) {
        self.apply_settings(ctx);
        self.save_settings();
    }

    fn change_font_size(&mut self, ctx: &egui::Context, delta: f32) {
        let size = if delta == 0.0 {
            Settings::default().font_size
        } else {
            self.settings.font_size + delta
        };
        self.settings.font_size = size.clamp(settings::MIN_FONT_SIZE, settings::MAX_FONT_SIZE);
        self.settings_changed(ctx);
    }

    // ---- terminal ------------------------------------------------------

    fn toggle_terminal(&mut self, ctx: &egui::Context) {
        self.terminal_open = !self.terminal_open;
        if self.terminal_open {
            self.start_terminal(ctx);
        }
    }

    /// Starts the shell if it isn't running, and focuses the terminal.
    fn start_terminal(&mut self, ctx: &egui::Context) {
        if self.terminal.is_none() {
            let cwd = self.workspace.as_ref().map(|ws| ws.root.path.as_path());
            match Terminal::spawn(self.settings.terminal_shell.as_deref(), cwd, ctx) {
                Ok(t) => self.terminal = Some(t),
                Err(e) => self.status = format!("Error: {e:#}"),
            }
        }
        self.focus_terminal = true;
    }

    fn terminal_panel(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let mut restart = false;
        let exited = self.terminal.as_mut().is_none_or(Terminal::has_exited);
        if !exited {
            // The PTY doesn't always report EOF when the shell exits, so poll for it.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
        ui.horizontal(|ui| {
            ui.strong("TERMINAL");
            if exited {
                ui.weak("— process exited");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui
                    .small_button("×")
                    .on_hover_text("Hide (Ctrl+`)")
                    .clicked();
                restart = ui
                    .small_button("⟳")
                    .on_hover_text("Restart shell")
                    .clicked();
            });
        });
        let font = TextStyle::Monospace.resolve(ui.style());
        let focus = std::mem::take(&mut self.focus_terminal);
        if let Some(terminal) = &mut self.terminal {
            terminal.ui(ui, &font, focus);
        }
        if exited && ui.input(|i| i.key_pressed(egui::Key::Enter)) && self.terminal.is_some() {
            restart = true;
        }
        if restart {
            self.terminal = None;
            self.start_terminal(&ui.ctx().clone());
        }
        if close {
            self.terminal_open = false;
        }
    }

    /// Opens the settings file in an editor tab, creating it with defaults if needed.
    fn open_settings_file(&mut self) {
        let Some(path) = self.settings_path.clone() else {
            self.status = "No config directory available on this platform".to_owned();
            return;
        };
        if !path.exists()
            && let Err(e) = self.settings.save(&path)
        {
            self.status = format!("Error: {e:#}");
            return;
        }
        self.open_file(&path);
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        let theme_names: Vec<String> = self.themes.iter().map(|t| t.name.clone()).collect();
        let mut changed = false;
        let mut open_file = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                changed = self
                    .settings
                    .ui(ui, &theme_names, updater::IS_RELEASE_BUILD);
                ui.separator();
                if let Some(path) = &self.settings_path {
                    ui.horizontal(|ui| {
                        open_file = ui.small_button("Edit as TOML").clicked();
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(path.display().to_string()).weak(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(path.display().to_string());
                    });
                }
            });
        self.settings_open = open && !ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if changed {
            self.settings_changed(ctx);
        }
        if open_file {
            self.open_settings_file();
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
                if response.clicked() {
                    // Restarting exits without running destructors; stop the shell first.
                    self.terminal = None;
                    if let Err(e) = updater::restart() {
                        self.status = format!("Could not restart: {e:#}");
                    }
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

    /// Makes `themes[idx]` the active theme for the UI and the highlighter.
    fn activate_theme(&mut self, ctx: &egui::Context, idx: usize) {
        self.theme = idx;
        let theme = &self.themes[idx];
        // Follow the color theme rather than the OS light/dark preference.
        ctx.set_theme(if theme.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        theme.apply(ctx);
        self.highlighter.set_theme(theme.syntect_theme());
    }

    /// Switches to `themes[idx]` and remembers the choice.
    fn set_theme(&mut self, ctx: &egui::Context, idx: usize) {
        self.settings.theme = self.themes[idx].name.clone();
        self.settings_changed(ctx);
        self.status = format!("Theme: {}", self.current_theme().name);
    }

    fn reload_themes(&mut self, ctx: &egui::Context) {
        let current = self.current_theme().name.clone();
        let (themes, errors) = theme::load_all(theme::themes_dir().as_deref());
        self.themes = themes;
        self.theme = self
            .themes
            .iter()
            .position(|t| t.name == current)
            .unwrap_or(0);
        // Also picks up a configured theme that previously failed to load.
        self.apply_settings(ctx);
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
        self.save_document(self.active);
    }

    /// Saves a document, asking for a path if it has none. Returns true on success.
    fn save_document(&mut self, idx: usize) -> bool {
        match self.documents.get(idx) {
            Some(doc) if doc.path.is_some() => {}
            Some(_) => return self.save_document_as(idx),
            None => return false,
        }
        let doc = &mut self.documents[idx];
        let result = doc.save();
        self.report_save(idx, result)
    }

    /// Saves a document under a new path chosen in a dialog. Returns true on success.
    fn save_document_as(&mut self, idx: usize) -> bool {
        let Some(doc) = self.documents.get_mut(idx) else {
            return false;
        };
        let mut dialog = rfd::FileDialog::new().set_file_name(doc.title());
        if let Some(dir) = doc.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else {
            return false;
        };
        let result = doc.save_as(path);
        self.report_save(idx, result)
    }

    fn report_save(&mut self, idx: usize, result: anyhow::Result<()>) -> bool {
        let doc = &self.documents[idx];
        match result {
            Ok(()) => {
                self.status = format!("Saved {}", doc.title());
                self.lsp.did_save(doc);
                if doc.path.is_some() && doc.path == self.settings_path {
                    self.settings_reload_pending = true;
                }
                true
            }
            Err(e) => {
                self.status = format!("Error: {e:#}");
                false
            }
        }
    }

    fn undo(&mut self) {
        if let Some(doc) = self.documents.get_mut(self.active) {
            if let Some(cursor) = doc.buffer.undo() {
                self.select(cursor..cursor, true);
            } else {
                self.status = "Nothing to undo".to_owned();
            }
        }
    }

    fn redo(&mut self) {
        if let Some(doc) = self.documents.get_mut(self.active) {
            if let Some(cursor) = doc.buffer.redo() {
                self.select(cursor..cursor, true);
            } else {
                self.status = "Nothing to redo".to_owned();
            }
        }
    }

    fn select(&mut self, range: Range<usize>, focus: bool) {
        self.pending_selection = Some(PendingSelection { range, focus });
    }

    fn open_find(&mut self, replace: bool) {
        let seed = self.documents.get(self.active).and_then(|doc| {
            let r = self.selection.clone();
            (!r.is_empty()).then(|| {
                egui::TextBuffer::char_range(&doc.buffer, r.start.into()..r.end.into()).to_owned()
            })
        });
        self.find.show(replace, seed);
    }

    /// Moves to the next/previous match relative to the cursor.
    fn find_step(&mut self, forward: bool) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if !self.find.open {
            self.open_find(false);
            return;
        }
        self.find.refresh(doc);
        let matches = self.find.matches();
        let idx = if forward {
            search::next_match(matches, self.selection.end.max(self.selection.start + 1))
        } else {
            search::prev_match(matches, self.selection.start)
        };
        // Starting right on a match (e.g. just opened with a selection) should not skip it.
        let idx = match (forward, self.find.current) {
            (true, None) => search::next_match(matches, self.selection.start),
            _ => idx,
        };
        if let Some(i) = idx {
            let range = matches[i].clone();
            self.find.current = Some(i);
            self.select(range, false);
        }
    }

    fn replace_current(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        self.find.refresh(doc);
        // Only replace when the current match is what's selected; otherwise just find it.
        let current = self
            .find
            .current
            .filter(|&i| self.find.matches().get(i) == Some(&self.selection));
        let Some(i) = current else {
            self.find_step(true);
            return;
        };
        let edits = self.find.replacements(doc, [i]);
        let placed = doc.buffer.replace_ranges(edits);
        if let Some(r) = placed.first() {
            self.selection = r.end..r.end;
            self.find.current = None;
            self.find.refresh(doc);
            self.find_step(true);
        }
    }

    fn replace_all(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        self.find.refresh(doc);
        let edits = self.find.replacements(doc, 0..self.find.matches().len());
        let count = edits.len();
        if count > 0 {
            doc.buffer.replace_ranges(edits);
        }
        self.status = format!(
            "Replaced {count} occurrence{}",
            if count == 1 { "" } else { "s" }
        );
    }

    fn handle_find_action(&mut self, action: FindAction) {
        match action {
            FindAction::Next => self.find_step(true),
            FindAction::Prev => self.find_step(false),
            FindAction::Replace => self.replace_current(),
            FindAction::ReplaceAll => self.replace_all(),
            FindAction::Close => {
                self.find.close();
                let sel = self.selection.clone();
                self.select(sel, true);
            }
        }
    }

    fn goto(&mut self, line: usize, col: usize) {
        if let Some(doc) = self.documents.get(self.active) {
            let at = doc.buffer.char_at_line_col(line, col);
            self.select(at..at, true);
        }
    }

    /// Whether keyboard input currently targets the active editor (or nothing),
    /// as opposed to some other text field with its own undo.
    fn editor_has_keyboard(&self, ctx: &egui::Context) -> bool {
        let focused = ctx.memory(|m| m.focused());
        focused.is_none() || focused == self.documents.get(self.active).map(|d| d.id)
    }

    /// Closes a tab, first asking what to do if it has unsaved changes.
    fn request_close_tab(&mut self, idx: usize) {
        match self.documents.get(idx) {
            Some(doc) if doc.is_dirty() => {
                self.active = idx;
                self.pending_close = Some(CloseRequest::Tab(doc.id));
            }
            Some(_) => self.close_tab(idx),
            None => {}
        }
    }

    /// Closes a tab unconditionally, discarding unsaved changes.
    fn close_tab(&mut self, idx: usize) {
        if idx < self.documents.len() {
            let doc = self.documents.remove(idx);
            self.lsp.did_close(&doc);
        }
        if self.documents.is_empty() {
            self.documents.push(Document::untitled());
        }
        if self.active >= self.documents.len() || self.active > idx {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// Runs a command, whether it came from a shortcut, a menu or the palette.
    fn run(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::NewFile => self.new_file(),
            Command::OpenFile => self.open_file_dialog(),
            Command::OpenFolder => self.open_folder_dialog(),
            Command::Save => self.save_active(),
            Command::SaveAs => {
                self.save_document_as(self.active);
            }
            Command::CloseTab => self.request_close_tab(self.active),
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Find => self.open_find(false),
            Command::Replace => self.open_find(true),
            Command::FindNext => self.find_step(true),
            Command::FindPrevious => self.find_step(false),
            Command::GoToLine => self.goto_line.show(),
            Command::NextTab => {
                self.active = (self.active + 1) % self.documents.len();
            }
            Command::PreviousTab => {
                self.active = self
                    .active
                    .checked_sub(1)
                    .unwrap_or(self.documents.len() - 1);
            }
            Command::ShowPalette => self.palette.show(),
            Command::OpenSettings => self.settings_open = true,
            Command::OpenSettingsFile => self.open_settings_file(),
            Command::ToggleWordWrap => {
                self.settings.word_wrap = !self.settings.word_wrap;
                self.settings_changed(ctx);
            }
            Command::ToggleLineNumbers => {
                self.settings.line_numbers = !self.settings.line_numbers;
                self.settings_changed(ctx);
            }
            Command::ToggleTerminal => self.toggle_terminal(ctx),
            Command::RestartTerminal => {
                self.terminal = None;
                self.terminal_open = true;
                self.start_terminal(ctx);
            }
            Command::TriggerCompletion => self.trigger_completion(),
            Command::GoToDefinition => self.go_to_definition(),
            Command::NextProblem => self.goto_problem(true),
            Command::PreviousProblem => self.goto_problem(false),
            Command::RestartLanguageServers => {
                self.lsp.restart();
                for doc in &mut self.documents {
                    doc.diagnostics.clear();
                }
                self.status = "Restarting language servers".to_owned();
            }
            Command::IncreaseFontSize => self.change_font_size(ctx, 1.0),
            Command::DecreaseFontSize => self.change_font_size(ctx, -1.0),
            Command::ResetFontSize => self.change_font_size(ctx, 0.0),
            Command::ReloadThemes => self.reload_themes(ctx),
            Command::OpenThemesFolder => self.open_themes_folder(),
            Command::CheckForUpdates => self.check_for_updates(ctx),
        }
    }

    /// Whether a command can run right now (greys out menu items).
    fn is_enabled(&self, command: Command) -> bool {
        let doc = self.documents.get(self.active);
        match command {
            Command::Undo => doc.is_some_and(|d| d.buffer.can_undo()),
            Command::Redo => doc.is_some_and(|d| d.buffer.can_redo()),
            Command::NextTab | Command::PreviousTab => self.documents.len() > 1,
            Command::CheckForUpdates => !matches!(
                self.updater.state(),
                UpdateState::Checking | UpdateState::Installing(_)
            ),
            _ => true,
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let editor_has_keyboard = self.editor_has_keyboard(ctx);
        let triggered: Vec<Command> = ctx.input_mut(|i| {
            Command::shortcut_table()
                .into_iter()
                // Leave e.g. Ctrl+Z to whichever other text field has focus.
                .filter(|(_, cmd)| editor_has_keyboard || !cmd.needs_editor_focus())
                .filter(|(shortcut, _)| i.consume_shortcut(shortcut))
                .map(|(_, cmd)| cmd)
                .collect()
        });
        for command in triggered {
            self.run(ctx, command);
        }
    }

    // ---- panels --------------------------------------------------------

    /// A menu entry for a command, with its shortcut hint. Runs it when clicked.
    fn menu_command(&mut self, ui: &mut egui::Ui, command: Command) {
        let mut button = egui::Button::new(command.menu_label());
        if let Some(shortcut) = command.shortcut() {
            button = button.shortcut_text(ui.ctx().format_shortcut(&shortcut));
        }
        if ui.add_enabled(self.is_enabled(command), button).clicked() {
            self.run(&ui.ctx().clone(), command);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        use Command::*;
        let menus: [(&str, &[&[Command]]); 3] = [
            (
                "File",
                &[
                    &[NewFile, OpenFile, OpenFolder],
                    &[Save, SaveAs, CloseTab],
                    &[Quit],
                ],
            ),
            (
                "Edit",
                &[
                    &[Undo, Redo],
                    &[Find, Replace, FindNext, FindPrevious],
                    &[TriggerCompletion],
                ],
            ),
            (
                "Go",
                &[
                    &[GoToLine, GoToDefinition],
                    &[NextProblem, PreviousProblem],
                    &[NextTab, PreviousTab],
                ],
            ),
        ];
        egui::MenuBar::new().ui(ui, |ui| {
            for (title, groups) in menus {
                ui.menu_button(title, |ui| {
                    for (i, group) in groups.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        for &command in *group {
                            self.menu_command(ui, command);
                        }
                    }
                });
            }
            ui.menu_button("View", |ui| {
                self.menu_command(ui, ShowPalette);
                ui.separator();
                self.menu_command(ui, ToggleWordWrap);
                self.menu_command(ui, ToggleLineNumbers);
                ui.separator();
                self.menu_command(ui, IncreaseFontSize);
                self.menu_command(ui, DecreaseFontSize);
                self.menu_command(ui, ResetFontSize);
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
                    self.menu_command(ui, ReloadThemes);
                    self.menu_command(ui, OpenThemesFolder);
                });
                ui.separator();
                self.menu_command(ui, ToggleTerminal);
                self.menu_command(ui, RestartTerminal);
                ui.separator();
                self.menu_command(ui, RestartLanguageServers);
                ui.separator();
                self.menu_command(ui, OpenSettings);
                self.menu_command(ui, OpenSettingsFile);
            });
            ui.menu_button("Help", |ui| {
                self.menu_command(ui, CheckForUpdates);
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
                    if let Some(status) = self.lsp.status(doc) {
                        ui.separator();
                        ui.weak(status);
                    }
                    let count = |s| doc.diagnostics.iter().filter(|d| d.severity == s).count();
                    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
                    if errors + warnings > 0 {
                        ui.separator();
                        ui.label(format!("⊗ {errors}  ⚠ {warnings}"))
                            .on_hover_text("Errors and warnings; F8 jumps to the next one");
                    }
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
            self.request_close_tab(idx);
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
        let line_numbers = self.settings.line_numbers;
        let gutter_width = if line_numbers {
            gutter::width(doc.line_count(), digit_width)
        } else {
            4.0
        };
        let desired_width = if self.settings.word_wrap {
            ui.available_width()
        } else {
            f32::INFINITY
        };
        let current_line = self.cursor.0;

        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = highlighter.highlight(text.as_str(), &extension, font.clone());
            job.wrap.max_width = wrap_width;
            ui.painter().layout_job(job)
        };

        // Apply a programmatic selection (undo/redo, find, go to line) before rendering.
        let reveal = self.pending_selection.take().map(|pending| {
            let range = pending.range;
            let mut state = egui::TextEdit::load_state(ui.ctx(), doc.id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(range.start),
                CCursor::new(range.end),
            )));
            egui::TextEdit::store_state(ui.ctx(), doc.id, state);
            if pending.focus {
                ui.memory_mut(|m| m.request_focus(doc.id));
            }
            CCursor::new(range.end)
        });
        let find = &self.find;
        let matches = if find.open { find.matches() } else { &[] };
        let mut hovered_problem = None;

        let mut output = egui::ScrollArea::both()
            .id_salt("editor_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                let output = egui::TextEdit::multiline(&mut doc.buffer)
                    .id(doc.id)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(desired_width)
                    .desired_rows(40)
                    .margin(egui::Margin {
                        left: gutter_width as i8,
                        right: 4,
                        top: 2,
                        bottom: 2,
                    })
                    .layouter(&mut layouter)
                    .show(ui);
                if line_numbers {
                    gutter::paint(ui, &output, current_line, &font);
                }
                paint_matches(ui, &output, matches, find.current);
                hovered_problem = language::paint_diagnostics(ui, &output, &doc.diagnostics);
                if let Some(cursor) = reveal {
                    let rect = output
                        .galley
                        .pos_from_cursor(cursor)
                        .translate(output.galley_pos.to_vec2());
                    ui.scroll_to_rect(rect.expand(8.0), Some(egui::Align::Center));
                }
                output
            })
            .inner;

        // Undo history lives in `Buffer`; drop egui's whole-text snapshots.
        output.state.clear_undoer();

        if let Some(message) = hovered_problem {
            output
                .response
                .response
                .clone()
                .on_hover_text_at_pointer(message);
        }

        if let Some(range) = output.cursor_range {
            let [start, end] = range.sorted_cursors();
            self.selection = start.index.into()..end.index.into();
            self.cursor_offset = range.primary.index.into();
            self.cursor = doc.line_col(self.cursor_offset);
            let caret = output
                .galley
                .pos_from_cursor(range.primary)
                .translate(output.galley_pos.to_vec2());
            self.cursor_screen_pos = ui
                .clip_rect()
                .contains(caret.left_bottom())
                .then(|| caret.left_bottom());
        }
    }

    /// Intercepts window close while there are unsaved changes.
    fn intercept_window_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_window_close {
            return;
        }
        if self.documents.iter().any(Document::is_dirty) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_close = Some(CloseRequest::Window);
        }
    }

    /// Shows the unsaved-changes prompt for a pending close, if any.
    fn close_prompt(&mut self, ctx: &egui::Context) {
        let Some(request) = self.pending_close else {
            return;
        };
        let dirty: Vec<String> = match request {
            CloseRequest::Tab(id) => self
                .documents
                .iter()
                .filter(|d| d.id == id && d.is_dirty())
                .map(Document::title)
                .collect(),
            CloseRequest::Window => self
                .documents
                .iter()
                .filter(|d| d.is_dirty())
                .map(Document::title)
                .collect(),
        };
        if dirty.is_empty() {
            // Saved or closed some other way in the meantime.
            self.pending_close = None;
            self.finish_close(ctx, request);
            return;
        }

        let modal = egui::Modal::new(egui::Id::unique("unsaved_changes")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            let (heading, save_label, discard_label) = match &dirty[..] {
                [one] => (
                    format!("Do you want to save the changes you made to {one}?"),
                    "Save",
                    "Don't Save",
                ),
                many => (
                    format!("Do you want to save changes to {} files?", many.len()),
                    "Save All",
                    "Discard All",
                ),
            };
            ui.label(egui::RichText::new(heading).strong());
            if dirty.len() > 1 {
                for title in &dirty {
                    ui.label(format!("• {title}"));
                }
            }
            ui.label("Your changes will be lost if you don't save them.");
            ui.add_space(8.0);
            let mut choice = None;
            ui.horizontal(|ui| {
                if ui.button(save_label).clicked() {
                    choice = Some(CloseChoice::Save);
                }
                if ui.button(discard_label).clicked() {
                    choice = Some(CloseChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(CloseChoice::Cancel);
                }
            });
            choice
        });
        let choice = if modal.should_close() {
            Some(CloseChoice::Cancel)
        } else {
            modal.inner
        };

        match choice {
            None => {}
            Some(CloseChoice::Cancel) => self.pending_close = None,
            Some(CloseChoice::Discard) => {
                self.pending_close = None;
                self.finish_close(ctx, request);
            }
            Some(CloseChoice::Save) => {
                self.pending_close = None;
                let targets: Vec<usize> = (0..self.documents.len())
                    .filter(|&i| {
                        let d = &self.documents[i];
                        d.is_dirty()
                            && (request == CloseRequest::Window
                                || CloseRequest::Tab(d.id) == request)
                    })
                    .collect();
                // Stop at the first failure (or cancelled save dialog) and keep everything open.
                if targets.into_iter().all(|i| self.save_document(i)) {
                    self.finish_close(ctx, request);
                }
            }
        }
    }

    fn finish_close(&mut self, ctx: &egui::Context, request: CloseRequest) {
        match request {
            CloseRequest::Tab(id) => {
                if let Some(idx) = self.documents.iter().position(|d| d.id == id) {
                    self.close_tab(idx);
                }
            }
            CloseRequest::Window => {
                self.allow_window_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Find bar above the editor; jumps to the first match as the query is typed.
    fn find_bar(&mut self, ui: &mut egui::Ui) {
        if !self.find.open {
            return;
        }
        if let Some(action) = self.find.ui(ui) {
            self.handle_find_action(action);
        }
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if self.find.refresh(doc)
            && let Some(i) = search::next_match(self.find.matches(), self.selection.start)
        {
            self.find.current = Some(i);
            let range = self.find.matches()[i].clone();
            self.select(range, false);
        }
        ui.separator();
    }
}

/// Highlights search matches inside the visible part of the editor.
fn paint_matches(
    ui: &egui::Ui,
    output: &egui::text_edit::TextEditOutput,
    matches: &[Range<usize>],
    current: Option<usize>,
) {
    if matches.is_empty() {
        return;
    }
    let clip = ui.clip_rect();
    let galley_pos = output.galley_pos;
    let origin = galley_pos.to_vec2();
    let galley = &output.galley;
    let first = galley.cursor_from_pos(clip.min - galley_pos).index.0;
    let last = galley.cursor_from_pos(clip.max - galley_pos).index.0;
    let start = matches.partition_point(|m| m.end < first);
    let accent = ui.visuals().selection.bg_fill;
    for (i, m) in matches.iter().enumerate().skip(start) {
        if m.start > last {
            break;
        }
        let a = galley
            .pos_from_cursor(CCursor::new(m.start))
            .translate(origin);
        let b = galley
            .pos_from_cursor(CCursor::new(m.end))
            .translate(origin);
        // Multi-line matches are marked on their first line only.
        let right = if (a.top() - b.top()).abs() < 1.0 {
            b.left()
        } else {
            a.left() + 8.0
        };
        let rect = egui::Rect::from_min_max(a.left_top(), egui::pos2(right, a.bottom()));
        let alpha = if Some(i) == current { 160 } else { 70 };
        ui.painter()
            .rect_filled(rect, 2.0, accent.gamma_multiply_u8(alpha));
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
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.settings_reload_pending) {
            self.reload_settings(&ctx);
        }
        self.intercept_window_close(&ctx);
        self.update_language_servers(&ctx);
        if !self.palette.open {
            if self.editor_has_keyboard(&ctx) {
                self.completion_keys(&ctx);
            }
            self.handle_shortcuts(&ctx);
        }
        self.poll_updater();
        let revision_before = self
            .documents
            .get(self.active)
            .map_or(0, |d| d.buffer.revision());

        let panel_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 6));
        egui::Panel::top("menu_bar")
            .frame(panel_frame)
            .show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status_bar")
            .frame(panel_frame)
            .show(ui, |ui| self.status_bar(ui));
        if self.terminal_open {
            egui::Panel::bottom("terminal")
                .resizable(true)
                .default_size(240.0)
                .min_size(80.0)
                .show(ui, |ui| self.terminal_panel(ui));
        }
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
                self.find_bar(ui);
                self.editor(ui);
            });
        self.after_editor_update(revision_before);
        self.completion_popup(&ctx);

        let line_count = self
            .documents
            .get(self.active)
            .map_or(1, Document::line_count);
        if let Some((line, col)) = self.goto_line.ui(ui.ctx(), line_count) {
            self.goto(line, col);
        }
        if let Some(command) = self.palette.ui(&ctx) {
            self.run(&ctx, command);
        }
        self.settings_window(&ctx);
        self.close_prompt(&ctx);
    }
}

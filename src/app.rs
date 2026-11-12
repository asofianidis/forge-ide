//! Top-level application state and UI layout.

mod ai_view;
mod editing;
mod explorer;
mod find_bar;
mod language;
mod pickers;
mod preferences;
mod search_panel;
mod session;
mod terminal_panel;

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::text::CCursor;
use egui::{Galley, Key, Modifiers, Rect, RichText, Shape, Stroke, TextStyle, pos2};

use crate::ai::Ai;
use crate::commands::Command;
use crate::editor::conflicts::{self, Resolution};
use crate::editor::find::Match;
use crate::editor::ops;
use crate::editor::scm::{self, EditorAction, EditorGit, Peek};
use crate::editor::{DiskEvent, DiskState, Document, Highlighter, gutter};
use crate::git::{self, PanelAction, ScmPanel, SourceControl};
use crate::github::model::{CheckOutcome, WorkflowRun};
use crate::github::{BranchContext, GhEvent, GitHub, ListAction, PrListPanel};
use crate::history::{HistoryPane, RebasePane};
use crate::lsp::{Feature, Lsp, ServerBrowser, ServerManager};
use crate::palette::Picker;
use crate::panes::{ChangesPane, CommitPane, LogPane, Pane, PaneAction};
use crate::search::FileIndex;
use crate::settings::{Session, Settings};
use crate::theme::{self, Theme, UiColors};
use crate::ui::activity::{self, SidebarView};
use crate::ui::icons::{self, Icon};
use crate::updater::{self, UpdateState, Updater};
use crate::watcher::FsWatcher;
use crate::workspace::{FileKinds, Workspace};

use editing::editor_id;
use explorer::ExplorerDialog;
use find_bar::FindBar;
use language::{EditorFrame, LanguageUi};
use pickers::{FileMatches, PickerMode};
use search_panel::SearchPanel;
use terminal_panel::TerminalPanel;

/// Actions from the merge conflict bar above the editor.
enum ConflictBarAction {
    AcceptAll(Resolution),
    Next,
    MarkResolved,
}

/// How often open files are checked for changes made by other programs.
const DISK_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Something waiting on the user to save or discard unsaved changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingClose {
    Tab(usize),
    Window,
}

/// The user's answer in the unsaved-changes dialog.
enum CloseChoice {
    Save,
    Discard,
    Cancel,
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
    /// Running language servers for the open files.
    lsp: Lsp,
    /// Hover, completion, rename and other language-feature UI state.
    language: LanguageUi,
    /// Languages whose "install a language server" hint the user dismissed.
    dismissed_hints: HashSet<&'static str>,
    settings: Settings,
    /// The Settings window is showing.
    settings_open: bool,
    updater: Updater,
    /// Update state seen last frame, to report transitions in the status bar.
    last_update_state: UpdateState,
    /// The current check was started from the menu, so "up to date" and
    /// errors are worth reporting.
    manual_update_check: bool,
    /// Commands chosen from menus or the palette, run at the end of the frame.
    pending_commands: Vec<Command>,
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
    /// Character to scroll into view (minimally) after an edit.
    reveal: Option<usize>,
    /// Unsaved-changes dialog being shown, if any.
    pending_close: Option<PendingClose>,
    /// Unsaved changes were saved or discarded, so the window may close.
    allow_close: bool,
    last_disk_poll: Instant,
    /// Window focus last frame, to re-check files when the user comes back.
    was_focused: bool,
    source_control: SourceControl,
    scm_panel: ScmPanel,
    github: GitHub,
    pr_panel: PrListPanel,
    sidebar_view: SidebarView,
    sidebar_open: bool,
    /// Diff, commit and pull request tabs, shown after the documents.
    panes: Vec<Pane>,
    /// When set, the central area shows this pane instead of a document.
    active_pane: Option<usize>,
    /// Change markers, blame and change peek for open files.
    editor_git: EditorGit,
    /// Move the cursor to (and scroll to) this 1-based line next frame.
    jump_to_line: Option<usize>,
    /// Requested jump to the next (+1) or previous (-1) change.
    change_nav: Option<i8>,
    egui_ctx: egui::Context,
    /// The current folder's tabs as last saved to the settings file.
    saved_session: Option<Session>,
    /// Reports changes other programs make in the workspace folder.
    watcher: Option<FsWatcher>,
    /// New / Rename / Delete dialog from the file tree.
    explorer_dialog: Option<ExplorerDialog>,
    /// Integrated terminal panel below the editor.
    terminal: TerminalPanel,
    /// BYO AI: agent threads and AI-written git text.
    ai: Ai,
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

        egui_extras::install_image_loaders(&cc.egui_ctx);
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 4.0);
            style.spacing.menu_margin = egui::Margin::same(8);
        });
        // Ctrl+= / Ctrl+- / Ctrl+0 change the editor font size instead of
        // zooming the whole UI.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        Self::apply_font_size(&cc.egui_ctx, settings.font_size);
        themes[idx].apply(&cc.egui_ctx);
        crate::ui::diff_style::set_theme(themes[idx].syntect_theme());

        let root = session::startup_folder(root, &settings)
            .filter(|p| p.is_dir())
            .map(|p| std::path::absolute(&p).unwrap_or(p));
        let workspace = root.map(Workspace::open);
        let folder = workspace.as_ref().map(|w| w.root.path.clone());
        let mut app = Self {
            source_control: SourceControl::new(cc.egui_ctx.clone(), folder.as_deref()),
            scm_panel: ScmPanel::default(),
            github: GitHub::new(cc.egui_ctx.clone(), folder.as_deref()),
            pr_panel: PrListPanel::default(),
            sidebar_view: SidebarView::Explorer,
            sidebar_open: true,
            panes: Vec::new(),
            active_pane: None,
            editor_git: EditorGit::default(),
            jump_to_line: None,
            change_nav: None,
            workspace,
            documents: vec![Document::untitled()],
            active: 0,
            highlighter: Highlighter::new(themes[idx].syntect_theme()),
            theme: idx,
            themes,
            status: theme_errors_status(&errors).unwrap_or_else(|| "Ready".to_owned()),
            cursor: (1, 1),
            servers: ServerManager::new(cc.egui_ctx.clone()),
            server_browser: ServerBrowser::default(),
            lsp: Lsp::new(cc.egui_ctx.clone()),
            language: LanguageUi::default(),
            dismissed_hints: HashSet::new(),
            settings,
            settings_open: false,
            updater,
            last_update_state: UpdateState::Idle,
            manual_update_check: false,
            pending_commands: Vec::new(),
            find: FindBar::default(),
            search: SearchPanel::default(),
            picker: Picker::default(),
            picker_mode: PickerMode::default(),
            file_index: FileIndex::default(),
            file_index_generation: 0,
            file_matches: FileMatches::default(),
            go_to_line: None,
            scroll_to: None,
            reveal: None,
            pending_close: None,
            allow_close: false,
            last_disk_poll: Instant::now(),
            was_focused: true,
            egui_ctx: cc.egui_ctx.clone(),
            saved_session: None,
            watcher: None,
            explorer_dialog: None,
            terminal: TerminalPanel::default(),
            ai: Ai::new(cc.egui_ctx.clone(), folder.as_deref()),
        };
        if let Some(folder) = folder {
            app.folder_opened(&folder);
        }
        app
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

    /// Makes `themes[idx]` the active theme without saving the choice.
    fn activate_theme(&mut self, ctx: &egui::Context, idx: usize) {
        self.theme = idx;
        let theme = &self.themes[idx];
        theme.apply(ctx);
        self.highlighter.set_theme(theme.syntect_theme());
        crate::ui::diff_style::set_theme(theme.syntect_theme());
    }

    /// Switches to `themes[idx]` and remembers the choice.
    fn set_theme(&mut self, ctx: &egui::Context, idx: usize) {
        self.activate_theme(ctx, idx);
        let name = self.themes[idx].name.clone();
        self.status = format!("Theme: {name}");
        self.update_settings(|s| s.theme = Some(name));
    }

    fn reload_themes(&mut self, ctx: &egui::Context) {
        let name = self.current_theme().name.clone();
        let (themes, errors) = theme::load_all(theme::themes_dir().as_deref());
        self.themes = themes;
        self.theme = self.themes.iter().position(|t| t.name == name).unwrap_or(0);
        let theme = &self.themes[self.theme];
        theme.apply(ctx);
        self.highlighter.set_theme(theme.syntect_theme());
        crate::ui::diff_style::set_theme(theme.syntect_theme());
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
        self.active_pane = None;
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
                // Replace a pristine untitled tab instead of piling up tabs.
                if let [only] = self.documents.as_slice()
                    && only.path.is_none()
                    && only.text().is_empty()
                {
                    self.documents.clear();
                }
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
            self.open_folder(path);
        }
    }

    fn open_folder(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        // Remember the tabs (and cursors) of the folder being left.
        self.persist_session(true);
        self.status = format!("Opened folder {}", path.display());
        self.source_control.open(Some(&path));
        self.github.open(Some(&path));
        self.ai.set_folder(Some(&path));
        self.panes.clear();
        self.active_pane = None;
        self.editor_git.clear();
        self.lsp.restart();
        self.language = LanguageUi::default();
        self.search = SearchPanel::default();
        self.workspace = Some(Workspace::open(path.clone()));
        self.folder_opened(&path);
    }

    /// Starts another Forge process with `path` open.
    fn open_folder_in_new_window(&mut self, path: &Path) {
        let result = std::env::current_exe()
            .map_err(anyhow::Error::from)
            .and_then(|exe| Ok(std::process::Command::new(exe).arg(path).spawn()?));
        self.status = match result {
            Ok(_) => format!("Opened {} in a new window", path.display()),
            Err(e) => format!("Error: couldn't open a new window: {e:#}"),
        };
    }

    /// Opens a repository-relative path from source control in the editor.
    fn open_repo_file(&mut self, rel: &str) {
        if let Some(path) = self.source_control.abs_path(rel) {
            self.open_file(&path);
        }
    }

    /// Shows `pane`, reusing an existing tab for the same content.
    fn open_pane(&mut self, pane: Pane) {
        let existing = self.panes.iter().position(|p| match (p, &pane) {
            (Pane::Changes(a), Pane::Changes(b)) => a.is_for(&b.entry.path, b.staged),
            (Pane::Commit(a), Pane::Commit(b)) => a.hash == b.hash,
            (Pane::PullRequest(a), Pane::PullRequest(b)) => a.number == b.number,
            (Pane::History(a), Pane::History(b)) => a.path == b.path,
            (Pane::Log(a), Pane::Log(b)) => a.key == b.key,
            (Pane::Rebase(_), Pane::Rebase(_)) => true,
            (Pane::Ai(a), Pane::Ai(b)) => a.id == b.id,
            _ => false,
        });
        match existing {
            Some(i) => {
                if let Pane::PullRequest(p) = &mut self.panes[i] {
                    p.reload();
                }
                self.active_pane = Some(i);
            }
            None => {
                self.panes.push(pane);
                self.active_pane = Some(self.panes.len() - 1);
            }
        }
    }

    /// Opens the history tab for the repository, or one path in it.
    fn open_history(&mut self, path: Option<String>) {
        if !self.source_control.is_ready() {
            self.status = "Not a git repository".to_owned();
            return;
        }
        // The repository root has an empty relative path.
        let path = path.filter(|p| !p.is_empty());
        self.open_pane(Pane::History(Box::new(HistoryPane::new(path))));
    }

    fn open_rebase(&mut self, base: Option<String>) {
        if self.source_control.status.operation.is_some() {
            self.status = "Finish or abort the operation in progress first".to_owned();
            return;
        }
        // A fresh plan each time: close any existing rebase tab.
        if let Some(i) = self.panes.iter().position(|p| matches!(p, Pane::Rebase(_))) {
            self.close_pane(i);
        }
        let pane = RebasePane::new(base, &self.source_control);
        self.open_pane(Pane::Rebase(Box::new(pane)));
    }

    fn open_pull_request(&mut self, number: u64) {
        match self.github.open_pr(number) {
            Some(pane) => self.open_pane(Pane::PullRequest(Box::new(pane))),
            None => self.status = "GitHub isn't connected for this folder".to_owned(),
        }
    }

    fn close_pane(&mut self, idx: usize) {
        if idx >= self.panes.len() {
            return;
        }
        self.panes.remove(idx);
        self.active_pane = match self.active_pane {
            Some(a) if a == idx => {
                if self.panes.is_empty() {
                    None
                } else {
                    Some(idx.min(self.panes.len() - 1))
                }
            }
            Some(a) if a > idx => Some(a - 1),
            other => other,
        };
    }

    fn show_sidebar(&mut self, view: SidebarView) {
        if self.sidebar_view == view && self.sidebar_open {
            self.sidebar_open = false;
        } else {
            self.sidebar_view = view;
            self.sidebar_open = true;
        }
        if view == SidebarView::PullRequests && self.github.is_ready() {
            self.github.refresh();
        }
    }

    /// Moves source control and GitHub messages into the status bar and
    /// reacts to cross-cutting events.
    fn handle_events(&mut self) {
        for event in self.source_control.take_events() {
            self.status = match event {
                git::Event::Done(msg) => msg,
                git::Event::Failed(msg) => format!("Error: {msg}"),
            };
        }
        let mut gh_events = self.github.take_events();
        for pane in &mut self.panes {
            if let Pane::PullRequest(p) = pane {
                gh_events.extend(p.take_events());
            }
        }
        for event in gh_events {
            match event {
                GhEvent::Done(msg) => self.status = msg,
                GhEvent::Failed(msg) => self.status = format!("Error: {msg}"),
                GhEvent::OpenPr(n) => self.open_pull_request(n),
                GhEvent::GitChanged => self.source_control.refresh(),
                GhEvent::PrsChanged => self.github.refresh(),
                GhEvent::OpenLog {
                    title,
                    run,
                    job,
                    failed_only,
                } => self.open_log(title, run, job, failed_only),
            }
        }
    }

    fn branch_context(&self) -> BranchContext {
        let sc = &self.source_control;
        let b = &sc.status.branch;
        BranchContext {
            branch: b.head.clone(),
            has_upstream: b.upstream.is_some(),
            remote: sc.default_remote(),
            last_subject: sc.log.first().map(|c| c.subject.clone()),
            ahead: b.ahead,
        }
    }

    /// Saves a document, asking for a path if it has none. Returns whether it
    /// was saved (not cancelled and no error).
    fn save(&mut self, idx: usize) -> bool {
        let Some(doc) = self.documents.get_mut(idx) else {
            return false;
        };
        if doc.path.is_none() {
            return self.save_as(idx);
        }
        let result = doc.save();
        self.finish_save(idx, result)
    }

    /// Saves a document under a path picked in the save dialog. The tab then
    /// follows the new path. Returns whether it was saved.
    fn save_as(&mut self, idx: usize) -> bool {
        let Some(doc) = self.documents.get(idx) else {
            return false;
        };
        let mut dialog = rfd::FileDialog::new();
        if let Some(path) = &doc.path {
            dialog = dialog.set_file_name(doc.title());
            if let Some(dir) = path.parent() {
                dialog = dialog.set_directory(dir);
            }
        } else if let Some(ws) = &self.workspace {
            dialog = dialog.set_directory(&ws.root.path);
        }
        let Some(path) = dialog.save_file() else {
            return false;
        };
        let path = std::path::absolute(&path).unwrap_or(path);
        let doc = &mut self.documents[idx];
        let old = doc.path.clone();
        let result = doc.save_as(path.clone());
        if result.is_err() {
            // Keep pointing at the old file rather than one never written.
            doc.path = old;
            return self.finish_save(idx, result);
        }
        if let Some(dir) = path.parent() {
            self.reload_tree_dir(dir);
        }
        self.finish_save(idx, result)
    }

    /// Save As for the active tab. A clean tab already showing the chosen
    /// file is closed, since the file now holds this tab's text.
    fn save_active_as(&mut self) {
        self.active_pane = None;
        if !self.save_as(self.active) {
            return;
        }
        let path = self.documents[self.active].path.clone();
        let mut i = self.documents.len();
        while i > 0 {
            i -= 1;
            let other = &self.documents[i];
            if i != self.active && other.path == path && !other.is_dirty() {
                self.remove_tab(i);
            }
        }
    }

    fn finish_save(&mut self, idx: usize, result: anyhow::Result<()>) -> bool {
        let doc = &self.documents[idx];
        let saved = match result {
            Ok(()) => {
                self.status = format!("Saved {}", doc.title());
                if let Some(path) = &doc.path {
                    self.lsp.did_save(path, doc.text());
                    if Settings::path().as_deref() == Some(path.as_path()) {
                        let ctx = self.egui_ctx.clone();
                        self.reload_settings(&ctx);
                    }
                }
                true
            }
            Err(e) => {
                self.status = format!("Error: {e:#}");
                false
            }
        };
        self.source_control.refresh();
        saved
    }

    /// Moves to the next or previous tab (files, then diff / history panes),
    /// wrapping around at either end.
    fn cycle_tab(&mut self, forward: bool) {
        let docs = self.documents.len();
        let current = self.active_pane.map_or(self.active, |p| docs + p);
        match cycle(current, docs + self.panes.len(), forward) {
            i if i < docs => {
                self.active = i;
                self.active_pane = None;
            }
            i => self.active_pane = Some(i - docs),
        }
    }

    /// Closes a tab, first asking what to do with unsaved changes.
    fn close_tab(&mut self, idx: usize) {
        match self.documents.get(idx) {
            Some(doc) if doc.is_dirty() => self.pending_close = Some(PendingClose::Tab(idx)),
            Some(_) => self.remove_tab(idx),
            None => {}
        }
    }

    fn remove_tab(&mut self, idx: usize) {
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

    /// Stops the window from closing while there are unsaved changes, and
    /// asks the user about them instead.
    fn intercept_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.has_unsaved_changes()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_close = Some(PendingClose::Window);
        }
    }

    /// Shows the unsaved-changes dialog and carries out the user's choice.
    fn close_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_close else {
            return;
        };
        let dirty: Vec<usize> = match pending {
            PendingClose::Tab(idx) => vec![idx],
            PendingClose::Window => (0..self.documents.len())
                .filter(|&i| self.documents[i].is_dirty())
                .collect(),
        };
        if dirty.is_empty() {
            // Everything got saved some other way in the meantime.
            self.pending_close = None;
            if pending == PendingClose::Window {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let (title, save_label) = match (pending, dirty.as_slice()) {
            (PendingClose::Tab(_), [idx]) | (PendingClose::Window, [idx]) => (
                format!(
                    "Save changes to {} before closing?",
                    self.documents[*idx].title()
                ),
                "Save",
            ),
            _ => (
                format!("{} files have unsaved changes.", dirty.len()),
                "Save All",
            ),
        };

        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::unique("unsaved_changes")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.heading(&title);
            if dirty.len() > 1 {
                for &idx in &dirty {
                    ui.label(format!("• {}", self.documents[idx].title()));
                }
            }
            ui.label("Your changes will be lost if you don't save them.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(save_label).clicked() {
                    choice = Some(CloseChoice::Save);
                }
                if ui.button("Don't Save").clicked() {
                    choice = Some(CloseChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(CloseChoice::Cancel);
                }
            });
        });
        if modal.should_close() && choice.is_none() {
            choice = Some(CloseChoice::Cancel);
        }
        let Some(choice) = choice else {
            return;
        };

        self.pending_close = None;
        let proceed = match choice {
            CloseChoice::Cancel => false,
            CloseChoice::Discard => true,
            // Stop at the first file that isn't saved (cancelled or failed);
            // the status bar says why.
            CloseChoice::Save => dirty.iter().all(|&idx| self.save(idx)),
        };
        if !proceed {
            return;
        }
        match pending {
            PendingClose::Tab(idx) => self.remove_tab(idx),
            PendingClose::Window => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Checks open files for changes made by other programs: files outside
    /// the watched folder periodically, and all of them when the window
    /// regains focus.
    fn poll_disk(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let regained_focus = focused && !self.was_focused;
        self.was_focused = focused;
        // The watcher reports changes inside the folder as they happen, so
        // the timer only covers files outside it (and focus changes all).
        let watched: Vec<Option<bool>> = self
            .documents
            .iter()
            .map(|d| d.path.as_deref().map(|p| self.is_watched(p)))
            .collect();
        if watched.contains(&Some(false)) {
            ctx.request_repaint_after(DISK_POLL_INTERVAL);
        } else if !regained_focus {
            return;
        }
        if !regained_focus && self.last_disk_poll.elapsed() < DISK_POLL_INTERVAL {
            return;
        }
        self.last_disk_poll = Instant::now();
        for (doc, watched) in self.documents.iter_mut().zip(watched) {
            if watched != Some(false) && !regained_focus {
                continue;
            }
            match doc.poll_disk() {
                Some(DiskEvent::Reloaded) => {
                    self.status = format!("Reloaded {} (changed on disk)", doc.title());
                }
                Some(DiskEvent::Conflict) => {
                    self.status = format!("{} changed on disk", doc.title());
                }
                Some(DiskEvent::Deleted) => {
                    self.status = format!("{} was deleted on disk", doc.title());
                }
                None => {}
            }
        }
    }

    /// Banner above the editor when the active file changed or vanished on disk.
    fn disk_banner(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let warning = ui.visuals().warn_fg_color;
        match doc.disk {
            DiskState::InSync => return,
            DiskState::Changed(_) => {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        warning,
                        "⚠ This file changed on disk, and you have unsaved changes here.",
                    );
                    if ui
                        .button("Reload")
                        .on_hover_text("Discard your changes and load the file from disk")
                        .clicked()
                    {
                        doc.reload_from_disk();
                    }
                    if ui
                        .button("Keep Mine")
                        .on_hover_text("Keep your version; saving overwrites the file")
                        .clicked()
                    {
                        doc.keep_local();
                    }
                });
            }
            DiskState::Deleted => {
                ui.colored_label(
                    warning,
                    "⚠ This file was deleted on disk. Save to create it again.",
                );
            }
        }
        ui.separator();
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let terminal_focused = self.terminal_has_focus(ctx);
        if self.picker.open || self.go_to_line.is_some() || terminal_focused {
            // Let the dialog (or the shell) handle Escape; other shortcuts still work.
        } else if self.find.open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.close_find(ctx);
        }
        self.handle_tab(ctx);
        self.handle_typing(ctx);

        // Editing shortcuts only apply when typing in the editor, so that
        // e.g. Alt+Up in the find field doesn't move lines.
        // A diff / PR / history tab hides the document, so it can't be edited.
        let editor_focused = self.active_pane.is_none()
            && (self.editor_has_focus(ctx)
                || (ctx.memory(|m| m.focused().is_none())
                    && !self.picker.open
                    && self.go_to_line.is_none()));
        for (cmd, shortcut) in Command::shortcut_table() {
            if cmd.edits_text() && !editor_focused {
                continue;
            }
            // In the terminal, keys without Cmd/Ctrl (F8, Alt+F5, …) and
            // plain-Ctrl ones (Ctrl+R, Ctrl+G on macOS) belong to the shell.
            if terminal_focused && cmd != Command::ToggleTerminal && !shortcut.modifiers.command {
                continue;
            }
            if ctx.input_mut(|i| i.consume_shortcut(&shortcut)) {
                // On macOS, Option+letter also types a character (Option+Z is
                // "Ω"); drop it so the shortcut doesn't edit the text too.
                let m = shortcut.modifiers;
                if m.alt && !(m.ctrl || m.command || m.mac_cmd) {
                    ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(_))));
                }
                self.run_command(ctx, cmd);
            }
        }
    }

    fn run_command(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::NewFile => self.new_file(),
            Command::OpenFile => self.open_file_dialog(),
            Command::OpenFolder => self.open_folder_dialog(),
            Command::OpenRecent => self.open_recent_picker(),
            Command::Save => self.save_active(),
            Command::SaveAs => self.save_active_as(),
            Command::CloseTab => match self.active_pane {
                Some(i) => self.close_pane(i),
                None => self.close_tab(self.active),
            },
            Command::RevealActiveFile => {
                match self.documents.get(self.active).and_then(|d| d.path.clone()) {
                    Some(path) => self.reveal(&path),
                    None => self.status = "Save the file first".to_owned(),
                }
            }
            Command::OpenSettings => self.settings_open = true,
            Command::OpenSettingsFile => self.open_settings_file(),
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::Undo => self.undo(ctx, false),
            Command::Redo => self.undo(ctx, true),
            Command::Find => {
                self.active_pane = None;
                self.open_find(ctx, false);
            }
            Command::Replace => {
                self.active_pane = None;
                self.open_find(ctx, true);
            }
            Command::FindInProject => self.open_project_search(ctx),
            Command::ToggleComment
            | Command::Indent
            | Command::Outdent
            | Command::DuplicateLine
            | Command::MoveLineUp
            | Command::MoveLineDown => self.edit_command(ctx, cmd),
            Command::QuickOpen => self.open_quick_open(ctx),
            Command::GoToLine => {
                self.active_pane = None;
                self.open_go_to_line();
            }
            Command::NextTab => self.cycle_tab(true),
            Command::PreviousTab => self.cycle_tab(false),
            Command::ShowCommandPalette => self.open_command_palette(),
            Command::ShowExplorer => self.show_sidebar(SidebarView::Explorer),
            Command::ShowSourceControl => self.show_sidebar(SidebarView::SourceControl),
            Command::ShowPullRequests => self.show_sidebar(SidebarView::PullRequests),
            Command::ShowAi => self.show_sidebar(SidebarView::Ai),
            Command::NewAiThread => {
                self.sidebar_view = SidebarView::Ai;
                self.sidebar_open = true;
                self.new_ai_thread();
            }
            Command::ToggleBlame => self.editor_git.blame_on = !self.editor_git.blame_on,
            Command::NextChange => self.change_nav = Some(1),
            Command::PreviousChange => self.change_nav = Some(-1),
            Command::FileHistory => {
                let rel = self
                    .documents
                    .get(self.active)
                    .and_then(|d| d.path.as_deref())
                    .and_then(|p| self.source_control.rel_path(p));
                self.open_history(rel);
            }
            Command::RepositoryHistory => self.open_history(None),
            Command::ToggleWordWrap => self.toggle_word_wrap(),
            Command::ToggleLineNumbers => self.toggle_line_numbers(),
            Command::IncreaseFontSize => self.change_font_size(ctx, Some(1.0)),
            Command::DecreaseFontSize => self.change_font_size(ctx, Some(-1.0)),
            Command::ResetFontSize => self.change_font_size(ctx, None),
            Command::SelectTheme => self.open_theme_picker(),
            Command::ReloadThemes => self.reload_themes(ctx),
            Command::OpenThemesFolder => self.open_themes_folder(),
            Command::LanguageServers => self.server_browser.open = true,
            Command::RestartLanguageServers => self.restart_language_servers(),
            Command::ToggleFormatOnSave => {
                let on = !self.settings.format_on_save;
                self.update_settings(|s| s.format_on_save = on);
                self.status = format!("Format on save {}", if on { "on" } else { "off" });
            }
            Command::TriggerSuggest => self.trigger_suggest(ctx),
            Command::RenameSymbol => self.open_rename(ctx),
            Command::FormatDocument => {
                self.format_document(false);
            }
            Command::GoToDefinition => self.go_to_definition(ctx),
            Command::FindReferences => self.find_references(ctx),
            Command::NextProblem => self.go_to_problem(ctx, true),
            Command::PreviousProblem => self.go_to_problem(ctx, false),
            Command::ShowProblems => self.show_problems(),
            Command::ToggleTerminal => self.toggle_terminal(ctx),
            Command::RestartTerminal => self.restart_terminal(ctx),
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
            Command::Undo | Command::Redo => {
                self.active_pane.is_none()
                    && self.documents.get(self.active).is_some_and(|d| {
                        if cmd == Command::Undo {
                            d.buffer.can_undo()
                        } else {
                            d.buffer.can_redo()
                        }
                    })
            }
            Command::NextTab | Command::PreviousTab => self.documents.len() + self.panes.len() > 1,
            _ => true,
        };
        if ui.add_enabled(enabled, button).clicked() {
            self.pending_commands.push(cmd);
            ui.close();
        }
    }

    /// A checkbox menu entry for a toggle command, showing its shortcut on hover.
    fn menu_checkbox(&mut self, ui: &mut egui::Ui, cmd: Command, value: &mut bool, text: &str) {
        let mut response = ui.checkbox(value, text);
        if let Some(s) = cmd.shortcuts().first() {
            response = response.on_hover_text(ui.ctx().format_shortcut(s));
        }
        if response.changed() {
            self.pending_commands.push(cmd);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        use Command::*;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                for cmd in [NewFile, OpenFile, OpenFolder] {
                    self.menu_item(ui, cmd);
                }
                self.open_recent_menu(ui);
                ui.separator();
                self.menu_item(ui, Save);
                self.menu_item(ui, SaveAs);
                self.menu_item(ui, CloseTab);
                ui.separator();
                self.menu_item(ui, RevealActiveFile);
                ui.separator();
                self.menu_item(ui, OpenSettings);
                self.menu_item(ui, OpenSettingsFile);
                ui.separator();
                self.menu_item(ui, Quit);
            });
            ui.menu_button("Edit", |ui| {
                self.menu_item(ui, Undo);
                self.menu_item(ui, Redo);
                ui.separator();
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
                ui.separator();
                for cmd in [TriggerSuggest, RenameSymbol, FormatDocument] {
                    self.menu_item(ui, cmd);
                }
            });
            ui.menu_button("Go", |ui| {
                self.menu_item(ui, QuickOpen);
                self.menu_item(ui, GoToLine);
                ui.separator();
                self.menu_item(ui, GoToDefinition);
                self.menu_item(ui, FindReferences);
                ui.separator();
                self.menu_item(ui, NextProblem);
                self.menu_item(ui, PreviousProblem);
                self.menu_item(ui, NextChange);
                self.menu_item(ui, PreviousChange);
                ui.separator();
                self.menu_item(ui, NextTab);
                self.menu_item(ui, PreviousTab);
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
                self.menu_item(ui, ShowSourceControl);
                self.menu_item(ui, ShowPullRequests);
                self.menu_item(ui, ShowAi);
                self.menu_item(ui, ShowProblems);
                ui.separator();
                let mut blame = self.editor_git.blame_on;
                let mut checkbox = ui.add(egui::Checkbox::new(&mut blame, "Blame"));
                if let Some(s) = ToggleBlame.shortcuts().first() {
                    checkbox = checkbox.on_hover_text(ui.ctx().format_shortcut(s));
                }
                if checkbox.changed() {
                    self.editor_git.blame_on = blame;
                }
                self.menu_item(ui, FileHistory);
                self.menu_item(ui, RepositoryHistory);
                ui.separator();
                let mut wrap = self.settings.word_wrap;
                self.menu_checkbox(ui, ToggleWordWrap, &mut wrap, "Word Wrap");
                let mut numbers = self.settings.line_numbers;
                self.menu_checkbox(ui, ToggleLineNumbers, &mut numbers, "Line Numbers");
                for cmd in [IncreaseFontSize, DecreaseFontSize, ResetFontSize] {
                    self.menu_item(ui, cmd);
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
                self.menu_item(ui, RestartLanguageServers);
                ui.separator();
                let mut on = self.settings.format_on_save;
                if ui
                    .checkbox(&mut on, "Format on Save")
                    .on_hover_text("Format with the language server before saving")
                    .changed()
                {
                    self.update_settings(|s| s.format_on_save = on);
                }
            });
            ui.menu_button("Terminal", |ui| {
                self.menu_item(ui, ToggleTerminal);
                self.menu_item(ui, RestartTerminal);
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
                    self.update_settings(|s| s.auto_update = Some(auto));
                }
                ui.separator();
                ui.label(format!("Forge IDE {}", updater::VERSION));
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        // Draw into a child sized to the panel and allocate exactly that
        // space, so wide content can never push the resizable panel wider.
        let rect = ui.available_rect_before_wrap();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        self.sidebar_contents(&mut child);
        ui.allocate_rect(rect, egui::Sense::hover());
    }

    fn sidebar_contents(&mut self, ui: &mut egui::Ui) {
        let colors = self.current_theme().ui.clone();
        match self.sidebar_view {
            SidebarView::Explorer => self.explorer(ui),
            SidebarView::Search => {
                ui.add_space(4.0);
                self.search_view(ui);
            }
            SidebarView::SourceControl => {
                ui.add_space(4.0);
                match self.scm_panel.show(ui, &mut self.source_control, &colors) {
                    Some(PanelAction::OpenDiff { entry, staged }) => {
                        self.open_pane(Pane::Changes(ChangesPane::new(entry, staged)));
                    }
                    Some(PanelAction::OpenFile(rel)) => self.open_repo_file(&rel),
                    Some(PanelAction::OpenCommit(hash)) => {
                        let pane = CommitPane::new(hash, &self.source_control);
                        self.open_pane(Pane::Commit(pane));
                    }
                    Some(PanelAction::CreatePullRequest) => {
                        self.sidebar_view = SidebarView::PullRequests;
                        self.pr_panel.start_create();
                    }
                    Some(PanelAction::OpenHistory) => self.open_history(None),
                    Some(PanelAction::OpenRebase) => self.open_rebase(None),
                    Some(PanelAction::OpenFolder(path)) => self.open_folder(path),
                    Some(PanelAction::OpenFolderInNewWindow(path)) => {
                        self.open_folder_in_new_window(&path)
                    }
                    Some(PanelAction::RevealPath(path)) => {
                        if let Err(e) = open_path(&path) {
                            self.status = format!("Error: {e:#}");
                        }
                    }
                    None => {}
                }
            }
            SidebarView::PullRequests => {
                ui.add_space(4.0);
                let branch = self.branch_context();
                match self.pr_panel.show(ui, &mut self.github, &branch, &colors) {
                    Some(ListAction::Open(n)) => self.open_pull_request(n),
                    Some(ListAction::GenerateText { base }) => {
                        let task = self.source_control.suggest_pr_text(&base);
                        self.pr_panel.set_generate_task(task);
                    }
                    None => {}
                }
            }
            SidebarView::Ai => self.ai_sidebar(ui),
        }
    }

    /// Branch, sync state and current PR at the left of the status bar.
    fn branch_indicator(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.source_control.branch_name() else {
            return;
        };
        let colors = self.current_theme().ui.clone();
        let b = self.source_control.status.branch.clone();
        let mut text = name;
        if b.behind > 0 {
            text.push_str(&format!(" {}↓", b.behind));
        }
        if b.ahead > 0 {
            text.push_str(&format!(" {}↑", b.ahead));
        }
        if let Some(op) = self.source_control.busy() {
            ui.spinner().on_hover_text(op);
        }
        let response = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                icons::label(ui, Icon::Branch, colors.text_muted.0, 14.0);
                ui.add(egui::Label::new(RichText::new(text).small()).sense(egui::Sense::click()))
            })
            .inner
            .on_hover_text(match &b.upstream {
                Some(u) => format!(
                    "Branch {} tracking {u}. Click for Source Control.",
                    b.display()
                ),
                None => format!(
                    "Branch {} (not published). Click for Source Control.",
                    b.display()
                ),
            });
        if response.clicked() {
            self.sidebar_view = SidebarView::SourceControl;
            self.sidebar_open = true;
        }
        if let Some(head) = &b.head
            && let Some(pr) = self.github.pr_for_branch(head)
        {
            let n = pr.number;
            if ui
                .link(RichText::new(format!("PR #{n}")).small())
                .on_hover_text(&pr.title)
                .clicked()
            {
                self.open_pull_request(n);
            }
        }
        self.ci_indicator(ui, &colors);
        ui.separator();
    }

    /// CI status for the checked-out commit, with a menu of its runs.
    fn ci_indicator(&mut self, ui: &mut egui::Ui, colors: &UiColors) {
        let Some(sha) = self.source_control.status.branch.oid.clone() else {
            return;
        };
        let runs: Vec<WorkflowRun> = self.github.runs_for(&sha).into_iter().cloned().collect();
        if runs.is_empty() {
            return;
        }
        let outcomes: Vec<CheckOutcome> = runs.iter().map(WorkflowRun::outcome).collect();
        let overall = if outcomes.contains(&CheckOutcome::Fail) {
            CheckOutcome::Fail
        } else if outcomes.contains(&CheckOutcome::Pending) {
            CheckOutcome::Pending
        } else {
            CheckOutcome::Pass
        };
        let (glyph, color, tip) = crate::github::check_glyph(overall, colors);
        let response = ui
            .add(
                egui::Label::new(RichText::new(format!("{glyph} CI")).small().color(color))
                    .sense(egui::Sense::click()),
            )
            .on_hover_text(format!("{tip} for this commit. Click for details."));
        let mut log = None;
        let mut rerun = None;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(340.0);
            for run in &runs {
                let (g, c, _) = crate::github::check_glyph(run.outcome(), colors);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(g).color(c));
                    ui.label(RichText::new(&run.workflow_name).strong());
                    let state = if run.status == "completed" {
                        run.conclusion.clone()
                    } else {
                        run.status.replace('_', " ")
                    };
                    ui.label(RichText::new(state).small().color(colors.text_muted.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Open").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&run.url));
                        }
                        let failed = run.outcome() == CheckOutcome::Fail;
                        if failed && ui.small_button("Re-run failed").clicked() {
                            rerun = Some(run.database_id);
                        }
                        if run.status == "completed"
                            && ui
                                .small_button(if failed { "Failed log" } else { "Log" })
                                .clicked()
                        {
                            log = Some((run.workflow_name.clone(), run.database_id, failed));
                        }
                    });
                });
            }
        });
        if let Some((name, id, failed)) = log {
            self.open_log(format!("Log: {name}"), id, None, failed);
        }
        if let Some(id) = rerun {
            self.github.rerun(id, true);
            self.status = "Re-running failed jobs…".to_owned();
        }
    }

    fn open_log(&mut self, title: String, run: u64, job: Option<u64>, failed_only: bool) {
        let Some(task) = self.github.load_log(run, job, failed_only) else {
            self.status = "GitHub isn't connected for this folder".to_owned();
            return;
        };
        let key = format!("{run}/{job:?}/{failed_only}");
        if let Some(i) = self
            .panes
            .iter()
            .position(|p| matches!(p, Pane::Log(l) if l.key == key))
        {
            self.close_pane(i);
        }
        self.open_pane(Pane::Log(LogPane::new(title, key, task)));
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.branch_indicator(ui);
            ui.add(egui::Label::new(&self.status).truncate());
            self.language_server_hint(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.update_indicator(ui);
                if let Some(doc) = self.documents.get(self.active) {
                    ui.label(doc.extension());
                    ui.separator();
                    ui.label(format!("Ln {}, Col {}", self.cursor.0, self.cursor.1));
                }
                ui.separator();
                self.language_status(ui);
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
        let mut pane_clicked = None;
        let mut pane_to_close = None;
        let colors = self.current_theme().ui.clone();
        egui::ScrollArea::horizontal()
            .id_salt("tab_bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (idx, doc) in self.documents.iter().enumerate() {
                        let selected = idx == self.active && self.active_pane.is_none();
                        let text = egui::RichText::new(doc.title()).color(if selected {
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
                            self.active_pane = None;
                        }
                        if response.middle_clicked() {
                            to_close = Some(idx);
                        }
                        let close =
                            tab_close_button(ui, &colors, response.rect, selected, doc.is_dirty());
                        if close.clicked() {
                            to_close = Some(idx);
                        }
                        ui.add_space(4.0);
                    }
                    for (idx, pane) in self.panes.iter().enumerate() {
                        let selected = self.active_pane == Some(idx);
                        icons::label(
                            ui,
                            pane.icon(),
                            if selected {
                                colors.accent.0
                            } else {
                                colors.text_muted.0
                            },
                            14.0,
                        );
                        let mut title = pane.title();
                        if title.chars().count() > 40 {
                            title = title.chars().take(38).collect::<String>() + "…";
                        }
                        let text = RichText::new(title).italics().color(if selected {
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
                            let r = response.rect;
                            ui.painter().hline(
                                r.x_range().shrink(6.0),
                                r.bottom() + 1.0,
                                egui::Stroke::new(2.0, colors.accent.0),
                            );
                        }
                        if response.clicked() {
                            pane_clicked = Some(idx);
                        }
                        if response.middle_clicked() {
                            pane_to_close = Some(idx);
                        }
                        let close = tab_close_button(ui, &colors, response.rect, selected, false);
                        if close.clicked() {
                            pane_to_close = Some(idx);
                        }
                        ui.add_space(4.0);
                    }
                });
            });
        if let Some(idx) = to_close {
            self.close_tab(idx);
        }
        if let Some(idx) = pane_clicked {
            self.active_pane = Some(idx);
        }
        if let Some(idx) = pane_to_close {
            self.close_pane(idx);
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        let colors = self.current_theme().ui.clone();
        let mark_colors = gutter::MarkColors {
            added: colors.added.0,
            modified: colors.modified.0,
            deleted: colors.deleted.0,
        };
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let id = editor_id(doc);
        let path = doc.path.clone();
        let sc = &self.source_control;
        let rel = path.as_deref().and_then(|p| sc.rel_path(p));
        let (changes, blame) = match &path {
            Some(p) => (
                self.editor_git.changes(sc, p, doc.text()),
                self.editor_git.blame(sc, p, doc.text()),
            ),
            None => (None, None),
        };
        let found_conflicts = if doc.text().contains("<<<<<<<") {
            conflicts::find(doc.text())
        } else {
            Vec::new()
        };
        let in_conflict = rel
            .as_deref()
            .and_then(|r| sc.status.find(r))
            .is_some_and(|e| e.conflict.is_some());
        let peek_idx = self
            .editor_git
            .peek
            .as_ref()
            .filter(|p| Some(&p.path) == path.as_ref())
            .map(|p| p.hunk);

        // Keyboard navigation between changes.
        let mut action = None;
        if let (Some(dir), Some(c)) = (self.change_nav.take(), &changes) {
            let line = peek_idx
                .and_then(|i| c.hunks.get(i))
                .map_or(self.cursor.0, |h| h.start);
            let idx = if dir > 0 {
                c.next_after(line)
            } else {
                c.prev_before(line)
            };
            match idx {
                Some(i) => action = Some(EditorAction::GoToChange(i)),
                None => self.status = "No changes in this file".to_owned(),
            }
        }
        if peek_idx.is_some() && ui.input(|i| i.key_pressed(Key::Escape)) {
            action = Some(EditorAction::ClosePeek);
        }

        if !found_conflicts.is_empty() || in_conflict {
            if let Some(bar) = conflict_bar(ui, found_conflicts.len(), &colors) {
                match bar {
                    ConflictBarAction::AcceptAll(how) => {
                        let doc = &mut self.documents[self.active];
                        let resolved = conflicts::resolve_all(doc.text(), how);
                        doc.set_text(&resolved);
                    }
                    ConflictBarAction::Next => {
                        let line = self.cursor.0;
                        let next = found_conflicts
                            .iter()
                            .find(|c| c.start + 1 > line)
                            .or(found_conflicts.first());
                        self.jump_to_line = next.map(|c| c.start + 1);
                    }
                    ConflictBarAction::MarkResolved => {
                        self.save_active();
                        if let Some(r) = &rel {
                            self.source_control.stage(vec![r.clone()]);
                        }
                    }
                }
            }
            ui.add_space(2.0);
        }
        if self.editor_git.blame_on
            && let Some(p) = &path
        {
            if let Some(e) = self.editor_git.blame_error(p) {
                ui.colored_label(colors.warning.0, format!("Blame unavailable: {e}"));
            } else if blame.is_none() && self.editor_git.blame_loading(p) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Loading blame…").small().weak());
                });
            }
        }

        let problems = path
            .as_deref()
            .map(|p| self.lsp.problems(p, self.documents[self.active].text()))
            .unwrap_or_default();
        let can_define = path
            .as_deref()
            .is_some_and(|p| self.lsp.supports(p, Feature::Definition));
        let mut lsp_frame = EditorFrame::default();

        let doc = &mut self.documents[self.active];
        let extension = doc.extension().to_owned();
        let highlighter = &mut self.highlighter;
        let font = TextStyle::Monospace.resolve(ui.style());
        let digit_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let line_numbers = self.settings.line_numbers;
        let word_wrap = self.settings.word_wrap;
        let gutter_width = gutter::width(doc.line_count(), digit_width, line_numbers);
        let current_line = self.cursor.0;
        let mut scroll_to = self.scroll_to.take();
        let reveal = self.reveal.take();
        let matches: &[Match] = if self.find.open {
            &self.find.matches
        } else {
            &[]
        };
        let current_match = self.find.current;

        let visuals = ui.visuals();
        let background = visuals.text_edit_bg_color();
        let accent = colors.accent.0;
        let hl = HighlightColors {
            find: accent.gamma_multiply(0.25),
            find_current: accent.gamma_multiply(0.55),
            bracket_fill: visuals.selection.bg_fill.gamma_multiply(0.35),
            bracket_stroke: Stroke::new(1.0, visuals.weak_text_color()),
        };

        // Move the cursor for "go to change / conflict".
        if let Some(line) = self.jump_to_line.take() {
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            let idx = scm::line_start_char(doc.text(), line);
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(CCursor::new(idx))));
            state.store(ui.ctx(), id);
            scroll_to = Some(idx);
        }

        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = highlighter.highlight(text.as_str(), &extension, font.clone());
            job.wrap.max_width = wrap_width;
            ui.painter().layout_job(job)
        };

        let mut peek_anchor = None;
        let editor_rect = ui.available_rect_before_wrap();
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

        let mut output = egui::ScrollArea::both()
            .id_salt("editor_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let blame_x = blame.as_ref().map(|_| {
                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(scm::BLAME_WIDTH, 1.0),
                            egui::Sense::hover(),
                        );
                        r.x_range()
                    });
                    let underlay = ui.painter().add(Shape::Noop);
                    // Wrapping lays the text out to the visible width; otherwise
                    // long lines extend and the area scrolls sideways.
                    let desired_width = if word_wrap {
                        ui.available_width()
                    } else {
                        f32::INFINITY
                    };
                    let output = egui::TextEdit::multiline(&mut doc.buffer)
                        .id(id)
                        .code_editor()
                        .lock_focus(true)
                        .desired_width(desired_width)
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
                            hl.find_current
                        } else {
                            hl.find
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
                            ops::matching_bracket(doc.text(), range.primary.index.into())
                    {
                        for c in [a, b] {
                            let r = range_rects(galley, c..c + 1)[0].translate(offset);
                            shapes.push(Shape::rect_filled(r, 2.0, hl.bracket_fill));
                            shapes.push(Shape::rect_stroke(
                                r,
                                2.0,
                                hl.bracket_stroke,
                                egui::StrokeKind::Inside,
                            ));
                        }
                    }
                    ui.painter().set(underlay, Shape::Vec(shapes));
                    language::paint_problems(ui, &output, &problems, &visible, &colors);
                    let response = &output.response.response;

                    // What the language features need: the symbol under the
                    // pointer, the caret, and what was typed.
                    let pointer = ui
                        .input(|i| i.pointer.hover_pos())
                        .filter(|_| response.contains_pointer())
                        .and_then(|p| language::char_at(&output, p));
                    if let Some((at, _)) = pointer
                        && can_define
                        && ui.input(|i| i.modifiers.command)
                        && doc
                            .text()
                            .chars()
                            .nth(at)
                            .is_some_and(crate::lsp::text::is_word_char)
                        && let Some(word) = crate::lsp::text::word_at(doc.text(), at)
                    {
                        language::underline(ui, &output, word, colors.link.0);
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        if ui.input(|i| i.pointer.primary_clicked()) {
                            lsp_frame.definition_at = Some(at);
                        }
                    }
                    lsp_frame.pointer = pointer;
                    if let Some(range) = output.cursor_range
                        && response.has_focus()
                    {
                        let c: usize = range.primary.index.into();
                        let r = galley.pos_from_cursor(CCursor::new(c)).translate(offset);
                        lsp_frame.caret = Some((c, r));
                    }
                    if response.changed() {
                        lsp_frame.typed = ui.input(|i| {
                            i.events.iter().rev().find_map(|e| match e {
                                egui::Event::Text(t) => t.chars().last(),
                                _ => None,
                            })
                        });
                    }

                    gutter::paint(
                        ui,
                        &output,
                        current_line,
                        &font,
                        line_numbers,
                        changes.as_ref().map(|c| (c, &mark_colors)),
                    );
                    if !found_conflicts.is_empty()
                        && let Some((c, how)) =
                            scm::paint_conflicts(ui, &output, &found_conflicts, &colors)
                    {
                        action = Some(EditorAction::Resolve(c, how));
                    }
                    if let (Some(x), Some(b)) = (blame_x, &blame)
                        && let Some(hash) = scm::paint_blame(ui, x, &output, b, &colors)
                    {
                        action = Some(EditorAction::OpenCommit(hash));
                    }
                    // Clicking a change marker opens the peek.
                    if let Some(c) = &changes {
                        let hover = ui
                            .input(|i| i.pointer.hover_pos())
                            .and_then(|p| gutter::marker_hit(&output, p))
                            .and_then(|l| c.hunk_at(l));
                        if hover.is_some() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if let Some(i) = hover
                            && ui.input(|i| i.pointer.primary_clicked())
                        {
                            action = Some(if peek_idx == Some(i) {
                                EditorAction::ClosePeek
                            } else {
                                EditorAction::GoToChange(i)
                            });
                        }
                        if let Some(h) = peek_idx.and_then(|i| c.hunks.get(i)) {
                            let last = h.start + h.len.max(1) - 1;
                            peek_anchor = gutter::line_rect(&output, last);
                        }
                    }
                    if let Some(c) = scroll_to {
                        let r = galley.pos_from_cursor(CCursor::new(c)).translate(offset);
                        ui.scroll_to_rect(r.expand(digit_width * 4.0), Some(egui::Align::Center));
                    } else if let Some(c) = reveal {
                        let r = galley.pos_from_cursor(CCursor::new(c)).translate(offset);
                        ui.scroll_to_rect(r.expand(digit_width * 2.0), None);
                    }
                    output
                })
                .inner
            })
            .inner;

        // Undo history lives in the document's `Buffer` (Ctrl+Z is handled as
        // a command before the widget sees it), so drop egui's own snapshots,
        // which copy the whole text.
        output.state.clear_undoer();

        if let Some(range) = output.cursor_range {
            self.cursor = doc.line_col(range.primary.index.into());
        }

        // The change peek popup.
        if let (Some(idx), Some(c), Some(anchor)) = (peek_idx, &changes, peek_anchor) {
            let can_stage = if doc.is_dirty() {
                Err("Save the file first")
            } else {
                Ok(())
            };
            let anchor =
                egui::Rect::from_min_max(egui::pos2(output.galley_pos.x, anchor.top()), anchor.max);
            if let Some(a) = scm::show_peek(
                ui.ctx(),
                anchor,
                editor_rect,
                c,
                idx,
                doc.text(),
                can_stage,
                &colors,
            ) {
                action = Some(a);
            }
        }

        match action {
            Some(EditorAction::Revert(h)) => {
                let reverted = crate::git::line_changes::revert(doc.text(), &h);
                doc.set_text(&reverted);
                self.editor_git.peek = None;
                self.status = "Reverted change (save to keep it)".to_owned();
            }
            Some(EditorAction::Stage(h)) => {
                if let Some(r) = rel {
                    self.source_control.stage_change(r, h.start, h.len);
                }
                self.editor_git.peek = None;
            }
            Some(EditorAction::GoToChange(i)) => {
                if let (Some(p), Some(h)) = (&path, changes.as_ref().and_then(|c| c.hunks.get(i))) {
                    self.jump_to_line = Some(h.start);
                    self.editor_git.peek = Some(Peek {
                        path: p.clone(),
                        hunk: i,
                    });
                }
            }
            Some(EditorAction::ClosePeek) => self.editor_git.peek = None,
            Some(EditorAction::OpenCommit(hash)) => {
                let pane = CommitPane::new(hash, &self.source_control);
                self.open_pane(Pane::Commit(pane));
            }
            Some(EditorAction::Resolve(c, how)) => {
                let resolved = conflicts::resolve(doc.text(), &c, how);
                doc.set_text(&resolved);
            }
            None => {}
        }
        lsp_frame.problems = problems;
        self.after_editor(ui.ctx(), lsp_frame);
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

/// Banner above a file with merge conflicts.
fn conflict_bar(ui: &mut egui::Ui, count: usize, colors: &UiColors) -> Option<ConflictBarAction> {
    let mut action = None;
    egui::Frame::new()
        .fill(colors.warning.0.gamma_multiply(0.12))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if count == 0 {
                    ui.label(
                        RichText::new("All conflicts in this file are resolved.")
                            .color(colors.added.0),
                    );
                    if ui
                        .button("Save & Mark Resolved")
                        .on_hover_text("Saves the file and stages it")
                        .clicked()
                    {
                        action = Some(ConflictBarAction::MarkResolved);
                    }
                    return;
                }
                ui.label(
                    RichText::new(format!("{count} merge conflict(s)"))
                        .strong()
                        .color(colors.warning.0),
                );
                if ui.small_button("Next Conflict").clicked() {
                    action = Some(ConflictBarAction::Next);
                }
                ui.separator();
                if ui.small_button("Accept All Current").clicked() {
                    action = Some(ConflictBarAction::AcceptAll(Resolution::Current));
                }
                if ui.small_button("Accept All Incoming").clicked() {
                    action = Some(ConflictBarAction::AcceptAll(Resolution::Incoming));
                }
                if ui.small_button("Accept All Both").clicked() {
                    action = Some(ConflictBarAction::AcceptAll(Resolution::Both));
                }
            });
        });
    action
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

/// The close button that trails a tab.
///
/// The × is faint on idle background tabs and brightens when the tab is
/// active or hovered; hovering the button itself gives it a rounded
/// highlight. Unsaved tabs show a dot in this slot that turns into the ×
/// on hover, so closing never requires hunting for a tiny glyph.
fn tab_close_button(
    ui: &mut egui::Ui,
    colors: &UiColors,
    tab_rect: Rect,
    selected: bool,
    dirty: bool,
) -> egui::Response {
    let size = egui::vec2(18.0, tab_rect.height().max(18.0));
    let (slot, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let tab_hovered = ui.rect_contains_pointer(tab_rect.union(slot));
    let hovered = response.hovered();
    if ui.is_rect_visible(slot) {
        let button = Rect::from_center_size(slot.center(), egui::vec2(18.0, 18.0));
        let painter = ui.painter();
        if hovered {
            // Red circle signals a destructive action; darken it while pressed.
            let fill = if response.is_pointer_button_down_on() {
                colors.error.0.lerp_to_gamma(egui::Color32::BLACK, 0.2)
            } else {
                colors.error.0
            };
            painter.circle_filled(button.center(), button.width() / 2.0, fill);
        }
        if dirty && !hovered {
            let color = if selected || tab_hovered {
                colors.text.0
            } else {
                colors.text_muted.0
            };
            painter.circle_filled(button.center(), 3.5, color);
        } else {
            let color = if hovered {
                egui::Color32::WHITE
            } else if selected {
                colors.text.0
            } else if tab_hovered {
                colors.text_muted.0
            } else {
                colors.text_muted.0.gamma_multiply(0.45)
            };
            icons::paint(painter, button.shrink(3.0), Icon::Close, color);
        }
    }
    let tip = if selected {
        let shortcut = Command::CloseTab.shortcuts()[0];
        format!("Close ({})", ui.ctx().format_shortcut(&shortcut))
    } else {
        "Close".to_owned()
    };
    response.on_hover_text(tip)
}

impl eframe::App for ForgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.intercept_close(&ctx);
        // The dialog is modal; keep shortcuts from acting behind it.
        if self.pending_close.is_none() && self.explorer_dialog.is_none() {
            self.completion_keys(&ctx);
            self.handle_shortcuts(&ctx);
        }
        self.servers.poll();
        self.sync_language_servers();
        self.handle_lsp_events(&ctx);
        self.poll_updater();
        self.search.search.poll();
        self.poll_watcher(&ctx);
        self.poll_disk(&ctx);
        self.source_control.poll();
        self.poll_ai();
        self.github
            .watch_commit(self.source_control.status.branch.oid.clone());
        self.github
            .poll(self.sidebar_open && self.sidebar_view == SidebarView::PullRequests);
        self.handle_events();

        let panel_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 6));
        egui::Panel::top("menu_bar")
            .frame(panel_frame)
            .show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status_bar")
            .frame(panel_frame)
            .show(ui, |ui| self.status_bar(ui));
        let colors = self.current_theme().ui.clone();
        egui::Panel::left("activity_bar")
            .frame(panel_frame.inner_margin(egui::Margin::ZERO))
            .resizable(false)
            .exact_size(activity::WIDTH)
            .show(ui, |ui| {
                let changes = self.source_control.status.change_count();
                let review_requests = self.github.badge_count();
                let agents = self.ai.running_count();
                activity::show(
                    ui,
                    &mut self.sidebar_view,
                    &mut self.sidebar_open,
                    |v| match v {
                        SidebarView::SourceControl => changes,
                        SidebarView::PullRequests => review_requests,
                        SidebarView::Ai => agents,
                        SidebarView::Explorer | SidebarView::Search => 0,
                    },
                    &colors,
                );
            });
        if self.sidebar_open {
            egui::Panel::left("sidebar")
                .frame(panel_frame.inner_margin(egui::Margin::same(10)))
                .resizable(true)
                .default_size(300.0)
                .size_range(200.0..=640.0)
                .show(ui, |ui| self.sidebar(ui));
        }
        if self.terminal.open {
            egui::Panel::bottom("terminal")
                .frame(panel_frame)
                .resizable(true)
                .default_size(240.0)
                .size_range(80.0..=f32::INFINITY)
                .show(ui, |ui| self.terminal_panel(ui));
        }

        let bg = self.highlighter.background();
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).fill(bg))
            .show(ui, |ui| {
                self.tab_bar(ui);
                ui.separator();
                let ai_thread = match self.active_pane.and_then(|i| self.panes.get(i)) {
                    Some(Pane::Ai(p)) => Some(p.id.clone()),
                    _ => None,
                };
                match self.active_pane {
                    Some(_) if ai_thread.is_some() => {
                        self.ai_thread_pane(ui, ai_thread.as_deref().unwrap_or_default());
                    }
                    Some(i) if i < self.panes.len() => {
                        let action = self.panes[i].show(ui, &mut self.source_control, &colors);
                        match action {
                            Some(PaneAction::OpenFile(rel)) => self.open_repo_file(&rel),
                            Some(PaneAction::OpenCommit(hash)) => {
                                let pane = CommitPane::new(hash, &self.source_control);
                                self.open_pane(Pane::Commit(pane));
                            }
                            Some(PaneAction::OpenRebase(base)) => self.open_rebase(base),
                            Some(PaneAction::Close) => self.close_pane(i),
                            None => {}
                        }
                    }
                    _ => {
                        self.disk_banner(ui);
                        self.find_bar(ui);
                        self.editor(ui);
                    }
                }
            });

        if self.server_browser.open {
            let kinds = self.project_file_kinds();
            self.server_browser.show(&ctx, &mut self.servers, &kinds);
        }

        self.settings_window(&ctx);
        self.show_picker(&ctx);
        self.go_to_line_dialog(&ctx);
        self.rename_dialog(&ctx);
        for cmd in std::mem::take(&mut self.pending_commands) {
            self.run_command(&ctx, cmd);
        }
        self.explorer_dialog(&ctx);
        self.close_dialog(&ctx);
        self.persist_session(false);
    }

    fn on_exit(&mut self) {
        self.ai.shutdown();
        self.persist_session(true);
    }
}

/// The index after (or before) `current` among `len` items, wrapping around.
fn cycle(current: usize, len: usize, forward: bool) -> usize {
    match (len, forward) {
        (0, _) => 0,
        (_, true) => (current + 1) % len,
        (_, false) => (current.min(len - 1) + len - 1) % len,
    }
}

#[cfg(test)]
mod tests {
    use super::cycle;

    #[test]
    fn tab_cycling_wraps() {
        assert_eq!(cycle(0, 3, true), 1);
        assert_eq!(cycle(2, 3, true), 0);
        assert_eq!(cycle(0, 3, false), 2);
        assert_eq!(cycle(2, 3, false), 1);
        assert_eq!(cycle(0, 1, true), 0);
        assert_eq!(cycle(0, 1, false), 0);
        assert_eq!(cycle(0, 0, true), 0);
    }
}

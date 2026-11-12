//! Top-level application state and UI layout.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Key, KeyboardShortcut, Modifiers, RichText, TextStyle};

use crate::editor::conflicts::{self, Resolution};
use crate::editor::scm::{self, EditorAction, EditorGit, Peek};
use crate::editor::{Document, Highlighter, gutter};
use crate::git::status::Change;
use crate::git::{self, PanelAction, ScmPanel, SourceControl};
use crate::github::model::{CheckOutcome, WorkflowRun};
use crate::github::{BranchContext, GhEvent, GitHub, ListAction, PrListPanel};
use crate::history::{HistoryPane, RebasePane};
use crate::lsp::{ServerBrowser, ServerManager};
use crate::panes::{ChangesPane, CommitPane, LogPane, Pane, PaneAction};
use crate::settings::Settings;
use crate::theme::{self, Theme, UiColors};
use crate::ui::activity::{self, SidebarView};
use crate::ui::icons::{self, Icon};
use crate::updater::{self, UpdateState, Updater};
use crate::workspace::{FileKinds, FileNode, Workspace};

const SHORTCUT_NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SHORTCUT_OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SHORTCUT_SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SHORTCUT_CLOSE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);
const SHORTCUT_EXPLORER: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::E);
const SHORTCUT_SCM: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::G);
const SHORTCUT_PRS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::P);

const SHORTCUT_BLAME: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::ALT), Key::B);
const SHORTCUT_NEXT_CHANGE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::ALT, Key::F5);
const SHORTCUT_PREV_CHANGE: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::ALT.plus(Modifiers::SHIFT), Key::F5);

/// Actions from the merge conflict bar above the editor.
enum ConflictBarAction {
    AcceptAll(Resolution),
    Next,
    MarkResolved,
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
    goto_line: Option<usize>,
    /// Requested jump to the next (+1) or previous (-1) change.
    change_nav: Option<i8>,
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
        crate::ui::diff_style::set_theme(themes[idx].syntect_theme());

        let workspace = root.filter(|p| p.is_dir()).map(Workspace::open);
        let folder = workspace.as_ref().map(|w| w.root.path.clone());
        Self {
            source_control: SourceControl::new(cc.egui_ctx.clone(), folder.as_deref()),
            scm_panel: ScmPanel::default(),
            github: GitHub::new(cc.egui_ctx.clone(), folder.as_deref()),
            pr_panel: PrListPanel::default(),
            sidebar_view: SidebarView::Explorer,
            sidebar_open: true,
            panes: Vec::new(),
            active_pane: None,
            editor_git: EditorGit::default(),
            goto_line: None,
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
            dismissed_hints: HashSet::new(),
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
        crate::ui::diff_style::set_theme(theme.syntect_theme());
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
                    && only.text.is_empty()
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
        self.status = format!("Opened folder {}", path.display());
        self.source_control.open(Some(&path));
        self.github.open(Some(&path));
        self.panes.clear();
        self.active_pane = None;
        self.editor_git.clear();
        self.workspace = Some(Workspace::open(path));
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
        self.source_control.refresh();
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
        let (explorer, scm, prs) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&SHORTCUT_EXPLORER),
                i.consume_shortcut(&SHORTCUT_SCM),
                i.consume_shortcut(&SHORTCUT_PRS),
            )
        });
        let (blame, next_change, prev_change) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&SHORTCUT_BLAME),
                i.consume_shortcut(&SHORTCUT_NEXT_CHANGE),
                i.consume_shortcut(&SHORTCUT_PREV_CHANGE),
            )
        });
        if blame {
            self.editor_git.blame_on = !self.editor_git.blame_on;
        }
        if next_change {
            self.change_nav = Some(1);
        }
        if prev_change {
            self.change_nav = Some(-1);
        }
        if explorer {
            self.show_sidebar(SidebarView::Explorer);
        }
        if scm {
            self.show_sidebar(SidebarView::SourceControl);
        }
        if prs {
            self.show_sidebar(SidebarView::PullRequests);
        }
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
            match self.active_pane {
                Some(i) => self.close_pane(i),
                None => self.close_tab(self.active),
            }
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
            ui.menu_button("View", |ui| {
                let ctx = ui.ctx().clone();
                for (view, shortcut) in [
                    (SidebarView::Explorer, SHORTCUT_EXPLORER),
                    (SidebarView::SourceControl, SHORTCUT_SCM),
                    (SidebarView::PullRequests, SHORTCUT_PRS),
                ] {
                    if ui
                        .add(
                            egui::Button::new(view.title())
                                .shortcut_text(ctx.format_shortcut(&shortcut)),
                        )
                        .clicked()
                    {
                        self.show_sidebar(view);
                        ui.close();
                    }
                }
                ui.separator();
                let mut blame = self.editor_git.blame_on;
                if ui
                    .add(egui::Checkbox::new(&mut blame, "Blame"))
                    .on_hover_text(ctx.format_shortcut(&SHORTCUT_BLAME))
                    .changed()
                {
                    self.editor_git.blame_on = blame;
                }
                if ui
                    .add(
                        egui::Button::new("Next Change")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_NEXT_CHANGE)),
                    )
                    .clicked()
                {
                    self.change_nav = Some(1);
                    ui.close();
                }
                if ui
                    .add(
                        egui::Button::new("Previous Change")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_PREV_CHANGE)),
                    )
                    .clicked()
                {
                    self.change_nav = Some(-1);
                    ui.close();
                }
                let active_path = self.documents.get(self.active).and_then(|d| d.path.clone());
                if ui
                    .add_enabled(active_path.is_some(), egui::Button::new("File History"))
                    .clicked()
                {
                    let rel = active_path.and_then(|p| self.source_control.rel_path(&p));
                    self.open_history(rel);
                    ui.close();
                }
                if ui.button("Repository History").clicked() {
                    self.open_history(None);
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
                    if ui.button("Reload Themes").clicked() {
                        self.reload_themes(ui.ctx());
                    }
                    if ui.button("Open Themes Folder").clicked() {
                        self.open_themes_folder();
                    }
                });
            });
            ui.menu_button("Tools", |ui| {
                if ui.button("Language Servers…").clicked() {
                    self.server_browser.open = true;
                    ui.close();
                }
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
        let mut history = None;
        let sc = &self.source_control;
        let colors = &self.themes[self.theme].ui;
        let decorate = |p: &Path| sc.decoration(p);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                file_tree(
                    ui,
                    &mut ws.root,
                    true,
                    &mut clicked,
                    &mut history,
                    &decorate,
                    colors,
                )
            });
        if let Some(path) = clicked {
            self.open_file(&path);
        }
        if let Some(path) = history {
            let rel = self.source_control.rel_path(&path);
            self.open_history(rel);
        }
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
                            self.active_pane = None;
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
                        if ui
                            .add(egui::Button::new("×").small().frame(false))
                            .on_hover_text("Close")
                            .clicked()
                        {
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
        let path = doc.path.clone();
        let sc = &self.source_control;
        let rel = path.as_deref().and_then(|p| sc.rel_path(p));
        let (changes, blame) = match &path {
            Some(p) => (
                self.editor_git.changes(sc, p, &doc.text),
                self.editor_git.blame(sc, p, &doc.text),
            ),
            None => (None, None),
        };
        let found_conflicts = if doc.text.contains("<<<<<<<") {
            conflicts::find(&doc.text)
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
                        doc.text = conflicts::resolve_all(&doc.text, how);
                    }
                    ConflictBarAction::Next => {
                        let line = self.cursor.0;
                        let next = found_conflicts
                            .iter()
                            .find(|c| c.start + 1 > line)
                            .or(found_conflicts.first());
                        self.goto_line = next.map(|c| c.start + 1);
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

        let doc = &mut self.documents[self.active];
        let extension = doc.extension().to_owned();
        let highlighter = &mut self.highlighter;
        let font = TextStyle::Monospace.resolve(ui.style());
        let digit_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let gutter_width = gutter::width(doc.line_count(), digit_width);
        let current_line = self.cursor.0;
        let editor_id = egui::Id::unique(("editor", self.active));

        // Move the cursor for "go to change / conflict".
        let mut scroll_line = None;
        if let Some(line) = self.goto_line.take() {
            let mut state =
                egui::text_edit::TextEditState::load(ui.ctx(), editor_id).unwrap_or_default();
            let idx = scm::line_start_char(&doc.text, line);
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(idx),
                )));
            state.store(ui.ctx(), editor_id);
            scroll_line = Some(line);
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
        // editor keeps focus, permanently in the selection stroke color.
        let frame = egui::Frame::NONE
            .fill(ui.visuals().text_edit_bg_color())
            .inner_margin(egui::Margin {
                left: gutter_width as i8,
                right: 4,
                top: 2,
                bottom: 2,
            });

        let output = egui::ScrollArea::both()
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
                    let output = egui::TextEdit::multiline(&mut doc.text)
                        .id(editor_id)
                        .code_editor()
                        .lock_focus(true)
                        .desired_width(f32::INFINITY)
                        .desired_rows(40)
                        .frame(frame)
                        .layouter(&mut layouter)
                        .show(ui);
                    gutter::paint(
                        ui,
                        &output,
                        current_line,
                        &font,
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
                    if let Some(line) = scroll_line
                        && let Some(r) = gutter::line_rect(&output, line)
                    {
                        ui.scroll_to_rect(r, Some(egui::Align::Center));
                    }
                    output
                })
                .inner
            })
            .inner;

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
                &doc.text,
                can_stage,
                &colors,
            ) {
                action = Some(a);
            }
        }

        match action {
            Some(EditorAction::Revert(h)) => {
                doc.text = crate::git::line_changes::revert(&doc.text, &h);
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
                    self.goto_line = Some(h.start);
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
                doc.text = conflicts::resolve(&doc.text, &c, how);
            }
            None => {}
        }
    }
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

/// Recursively renders a directory tree. Sets `clicked` when a file is
/// selected. Names are colored by their git state.
fn file_tree(
    ui: &mut egui::Ui,
    node: &mut FileNode,
    is_root: bool,
    clicked: &mut Option<PathBuf>,
    history: &mut Option<PathBuf>,
    decorate: &dyn Fn(&Path) -> Option<Change>,
    colors: &UiColors,
) {
    let change = decorate(&node.path);
    let mut name = RichText::new(&node.name);
    if let Some(c) = change {
        name = name.color(git::panel_change_color(c, colors));
        if c == Change::Deleted {
            name = name.strikethrough();
        }
    }
    if node.is_dir {
        let header = egui::CollapsingHeader::new(name)
            .id_salt(&node.path)
            .default_open(is_root)
            .show(ui, |ui| {
                for child in node.children() {
                    file_tree(ui, child, false, clicked, history, decorate, colors);
                }
            });
        header.header_response.context_menu(|ui| {
            if ui.button("Refresh").clicked() {
                node.refresh();
                ui.close();
            }
            if ui.button("Show History").clicked() {
                *history = Some(node.path.clone());
                ui.close();
            }
        });
    } else {
        let response = ui
            .horizontal(|ui| {
                let r = ui.selectable_label(false, name);
                if let Some(c) = change {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(c.letter())
                                .small()
                                .color(git::panel_change_color(c, colors)),
                        )
                        .on_hover_text(c.describe());
                    });
                }
                r
            })
            .inner;
        if response.clicked() {
            *clicked = Some(node.path.clone());
        }
        response.context_menu(|ui| {
            if ui.button("Open").clicked() {
                *clicked = Some(node.path.clone());
                ui.close();
            }
            if ui.button("Show History").clicked() {
                *history = Some(node.path.clone());
                ui.close();
            }
        });
    }
}

impl eframe::App for ForgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(&ui.ctx().clone());
        self.servers.poll();
        self.poll_updater();
        self.source_control.poll();
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
                activity::show(
                    ui,
                    &mut self.sidebar_view,
                    &mut self.sidebar_open,
                    |v| match v {
                        SidebarView::SourceControl => changes,
                        SidebarView::PullRequests => review_requests,
                        SidebarView::Explorer => 0,
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

        let bg = self.highlighter.background();
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).fill(bg))
            .show(ui, |ui| {
                self.tab_bar(ui);
                ui.separator();
                match self.active_pane {
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
                    _ => self.editor(ui),
                }
            });

        if self.server_browser.open {
            let kinds = self.project_file_kinds();
            self.server_browser
                .show(&ui.ctx().clone(), &mut self.servers, &kinds);
        }
    }
}

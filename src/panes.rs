//! Non-editor tabs in the main area: a file's working-tree/staged diff, a
//! commit, and a GitHub pull request.

use std::collections::BTreeSet;

use egui::{RichText, Ui};

use crate::git::diff::{DiffLine, FileDiff, LineKind};
use crate::git::status::Entry;
use crate::git::{CommitDetail, SourceControl};
use crate::github::PrPane;
use crate::history::{HistoryAction, HistoryPane, RebasePane};
use crate::task::{self, Pending};
use crate::theme::UiColors;
use crate::ui::diff_view::{self, DiffColors, DiffHooks, LineRef};
use crate::ui::fmt;
use crate::ui::icons::{self, Icon};

/// Something a pane wants the app to do.
pub enum PaneAction {
    /// Open a repository-relative file in the editor.
    OpenFile(String),
    OpenCommit(String),
    /// Open the interactive rebase tab (onto this base, or the fork point).
    OpenRebase(Option<String>),
    /// Close the pane that returned this.
    Close,
}

pub enum Pane {
    Changes(ChangesPane),
    Commit(CommitPane),
    PullRequest(Box<PrPane>),
    History(Box<HistoryPane>),
    Rebase(Box<RebasePane>),
    Log(LogPane),
    /// A BYO AI thread; drawn by the app, which owns the threads.
    Ai(AiPane),
}

/// The tab of an AI thread.
pub struct AiPane {
    pub id: String,
    /// Kept in sync with the thread's title by the app.
    pub title: String,
}

impl Pane {
    pub fn title(&self) -> String {
        match self {
            Pane::Changes(p) => {
                let name = p.entry.file_name();
                if p.staged {
                    format!("{name} (Index)")
                } else {
                    format!("{name} (Working Tree)")
                }
            }
            Pane::Commit(p) => format!("Commit {}", &p.hash[..p.hash.len().min(7)]),
            Pane::PullRequest(p) => p.title(),
            Pane::History(p) => p.title(),
            Pane::Rebase(p) => p.title(),
            Pane::Log(p) => p.title.clone(),
            Pane::Ai(p) => p.title.clone(),
        }
    }

    pub fn icon(&self) -> Icon {
        match self {
            Pane::Changes(_) => Icon::Branch,
            Pane::Commit(_) => Icon::Commit,
            Pane::PullRequest(_) => Icon::PullRequest,
            Pane::History(_) | Pane::Rebase(_) => Icon::History,
            Pane::Log(_) => Icon::OpenFile,
            Pane::Ai(_) => Icon::Sparkle,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        sc: &mut SourceControl,
        colors: &UiColors,
    ) -> Option<PaneAction> {
        match self {
            Pane::Changes(p) => p.show(ui, sc, colors),
            Pane::Commit(p) => p.show(ui, colors),
            Pane::PullRequest(p) => {
                p.set_local(sc.root(), sc.status.branch.head.clone());
                p.show(ui, colors);
                None
            }
            Pane::History(p) => p.show(ui, sc, colors).map(history_action),
            Pane::Rebase(p) => p.show(ui, sc, colors).map(history_action),
            Pane::Log(p) => {
                p.show(ui, colors);
                None
            }
            Pane::Ai(_) => None,
        }
    }
}

fn history_action(a: HistoryAction) -> PaneAction {
    match a {
        HistoryAction::OpenCommit(h) => PaneAction::OpenCommit(h),
        HistoryAction::OpenRebase(b) => PaneAction::OpenRebase(b),
        HistoryAction::Close => PaneAction::Close,
    }
}

/// Plain-text output of a command (CI job logs), with a filter box.
pub struct LogPane {
    pub title: String,
    /// Identifies the log so reopening reuses the tab.
    pub key: String,
    text: Option<Result<String, String>>,
    task: Option<Pending<Result<String, String>>>,
    filter: String,
}

impl LogPane {
    pub fn new(title: String, key: String, task: Pending<Result<String, String>>) -> Self {
        Self {
            title,
            key,
            text: None,
            task: Some(task),
            filter: String::new(),
        }
    }

    fn show(&mut self, ui: &mut Ui, colors: &UiColors) {
        if let Some(r) = task::poll(&mut self.task) {
            self.text = Some(r);
        }
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&self.title).strong().size(15.0));
                    if self.task.is_some() {
                        ui.spinner();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(Ok(t)) = &self.text
                            && ui.button("Copy").clicked()
                        {
                            ui.ctx().copy_text(t.clone());
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filter)
                                .hint_text("Filter lines")
                                .desired_width(200.0),
                        );
                    });
                });
            });
        ui.separator();
        let text = match &self.text {
            Some(Ok(t)) => t,
            Some(Err(e)) => {
                ui.colored_label(colors.error.0, e);
                return;
            }
            None => return,
        };
        let filter = self.filter.to_lowercase();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| filter.is_empty() || l.to_lowercase().contains(&filter))
            .collect();
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
        egui::ScrollArea::both()
            .id_salt(("log", &self.key))
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, row_h, lines.len(), |ui, range| {
                for line in &lines[range] {
                    // gh prefixes each line with "job\tstep\ttimestamp"; keep the message.
                    let msg = line.rsplit('\t').next().unwrap_or(line);
                    let lower = msg.to_lowercase();
                    // "0 failed" in a passing summary isn't a failure.
                    let failed = lower.contains("failed") && !lower.contains(" 0 failed");
                    let color = if lower.contains("error") || failed || lower.contains("panicked") {
                        colors.error.0
                    } else if lower.contains("warning") {
                        colors.warning.0
                    } else {
                        colors.text.0
                    };
                    ui.add(
                        egui::Label::new(RichText::new(msg).monospace().color(color))
                            .extend()
                            .selectable(true),
                    )
                    .on_hover_text(*line);
                }
            });
    }
}

/// Diff of one changed file, against the index (`staged`) or work tree.
pub struct ChangesPane {
    pub entry: Entry,
    pub staged: bool,
    files: Option<Result<Vec<FileDiff>, String>>,
    task: Option<Pending<Result<Vec<FileDiff>, String>>>,
    /// Source control revision the diff was loaded at.
    loaded_rev: Option<u64>,
    /// Lines picked for staging, unstaging or discarding.
    selection: BTreeSet<LineRef>,
    /// Where a Shift+click range starts.
    anchor: Option<LineRef>,
}

impl ChangesPane {
    pub fn new(entry: Entry, staged: bool) -> Self {
        Self {
            entry,
            staged,
            files: None,
            task: None,
            loaded_rev: None,
            selection: BTreeSet::new(),
            anchor: None,
        }
    }

    /// Whether this pane shows `path` from the same side.
    pub fn is_for(&self, path: &str, staged: bool) -> bool {
        self.entry.path == path && self.staged == staged
    }

    fn show(
        &mut self,
        ui: &mut Ui,
        sc: &mut SourceControl,
        colors: &UiColors,
    ) -> Option<PaneAction> {
        if let Some(result) = task::poll(&mut self.task) {
            // Line numbers may have shifted, so a selection no longer applies.
            if self.files.as_ref() != Some(&result) {
                self.selection.clear();
                self.anchor = None;
            }
            self.files = Some(result);
        }
        // Reload whenever the repository changed (staging, edits, commits).
        if self.task.is_none() && self.loaded_rev != Some(sc.revision) {
            // Pick up the file's current status (e.g. it became staged).
            if let Some(e) = sc.status.find(&self.entry.path) {
                self.entry = e.clone();
            }
            self.loaded_rev = Some(sc.revision);
            self.task = sc.load_file_diff(&self.entry, self.staged);
        }

        let mut action = None;
        let mut switch_side = false;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&self.entry.path).strong().size(15.0));
                    let side = if self.staged {
                        "Staged changes"
                    } else {
                        "Working tree changes"
                    };
                    ui.label(RichText::new(side).color(colors.text_muted.0));
                    if self.task.is_some() {
                        ui.spinner();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        diff_view::layout_toggle(ui);
                        ui.separator();
                        let other = if self.staged {
                            "Show Working Tree"
                        } else {
                            "Show Staged"
                        };
                        let has_other = if self.staged {
                            self.entry.unstaged.is_some()
                        } else {
                            self.entry.staged.is_some()
                        };
                        if has_other && ui.button(other).clicked() {
                            switch_side = true;
                        }
                        if self.entry.unstaged != Some(crate::git::status::Change::Deleted)
                            && ui.button("Open File").clicked()
                        {
                            action = Some(PaneAction::OpenFile(self.entry.path.clone()));
                        }
                        if self.staged {
                            if ui.button("Unstage File").clicked() {
                                sc.unstage(vec![self.entry.path.clone()]);
                            }
                        } else if ui.button("Stage File").clicked() {
                            sc.stage(vec![self.entry.path.clone()]);
                        }
                    });
                });
            });
        ui.separator();
        if switch_side {
            self.staged = !self.staged;
            self.loaded_rev = None;
            self.files = None;
            self.selection.clear();
            self.anchor = None;
        }
        if !self.entry.is_untracked() {
            let hint = if self.selection.is_empty() {
                "Tip: click changed lines to stage just those (Shift+click selects a range)."
                    .to_owned()
            } else {
                format!(
                    "{} line(s) selected — use the buttons on the hunk header. Esc clears.",
                    self.selection.len()
                )
            };
            ui.label(RichText::new(hint).small().color(colors.text_muted.0));
            if !self.selection.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selection.clear();
                self.anchor = None;
            }
        }

        let dc = DiffColors::from_theme(colors);
        let mut hooks = LocalHooks {
            staged: self.staged,
            untracked: self.entry.is_untracked(),
            requests: Vec::new(),
            selection: &mut self.selection,
            anchor: &mut self.anchor,
        };
        egui::ScrollArea::both()
            .id_salt(("changes", &self.entry.path, self.staged))
            .auto_shrink(false)
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| match &self.files {
                        Some(Ok(files)) => {
                            if files.is_empty() {
                                ui.add_space(12.0);
                                let msg = if self.staged {
                                    "No staged changes in this file."
                                } else {
                                    "No unstaged changes in this file."
                                };
                                ui.label(RichText::new(msg).weak());
                            } else {
                                let id = ui.make_persistent_id((
                                    "changes",
                                    &self.entry.path,
                                    self.staged,
                                ));
                                diff_view::show(ui, id, files, &dc, &mut hooks, None);
                            }
                        }
                        Some(Err(e)) => {
                            ui.colored_label(colors.error.0, e);
                        }
                        None => {
                            ui.spinner();
                        }
                    });
            });
        for (label, patch, cached, reverse) in hooks.requests {
            sc.apply_patch(label, patch, cached, reverse);
        }
        action
    }
}

/// Stage / unstage / discard individual hunks or selected lines.
struct LocalHooks<'a> {
    staged: bool,
    untracked: bool,
    /// (label, patch, cached, reverse)
    requests: Vec<(&'static str, String, bool, bool)>,
    selection: &'a mut BTreeSet<LineRef>,
    anchor: &'a mut Option<LineRef>,
}

impl DiffHooks for LocalHooks<'_> {
    fn can_click(&self, line: &DiffLine) -> bool {
        !self.untracked && line.kind != LineKind::Context
    }

    fn click_hint(&self) -> &'static str {
        if self.staged {
            "Click to select lines to unstage (Shift+click for a range)"
        } else {
            "Click to select lines to stage or discard (Shift+click for a range)"
        }
    }

    fn is_selected(&self, at: LineRef) -> bool {
        self.selection.contains(&at)
    }

    fn line_clicked(&mut self, at: LineRef, file: &FileDiff, _line: &DiffLine, shift: bool) {
        match *self.anchor {
            Some(a) if shift && a.file == at.file && a.hunk == at.hunk => {
                // Select every changed line between the anchor and here.
                let (lo, hi) = (a.line.min(at.line), a.line.max(at.line));
                for (i, l) in file.hunks[at.hunk].lines.iter().enumerate() {
                    if (lo..=hi).contains(&i) && l.kind != LineKind::Context {
                        self.selection.insert(LineRef { line: i, ..at });
                    }
                }
            }
            _ => {
                if !self.selection.remove(&at) {
                    self.selection.insert(at);
                }
                *self.anchor = Some(at);
            }
        }
    }

    fn hunk_actions(&mut self, ui: &mut Ui, file_idx: usize, hunk_idx: usize, file: &FileDiff) {
        if self.untracked {
            return;
        }
        let picked: Vec<usize> = self
            .selection
            .iter()
            .filter(|r| r.file == file_idx && r.hunk == hunk_idx)
            .map(|r| r.line)
            .collect();
        if !picked.is_empty() {
            let n = picked.len();
            let mut done = false;
            if self.staged {
                if ui.small_button(format!("Unstage {n} Line(s)")).clicked()
                    && let Some(p) = file.lines_patch(hunk_idx, &picked, true)
                {
                    self.requests.push(("Unstage lines", p, true, true));
                    done = true;
                }
            } else {
                if ui.small_button(format!("Stage {n} Line(s)")).clicked()
                    && let Some(p) = file.lines_patch(hunk_idx, &picked, false)
                {
                    self.requests.push(("Stage lines", p, true, false));
                    done = true;
                }
                if ui
                    .small_button(format!("Discard {n} Line(s)"))
                    .on_hover_text("Revert just these lines in the working tree")
                    .clicked()
                    && let Some(p) = file.lines_patch(hunk_idx, &picked, true)
                {
                    self.requests.push(("Discard lines", p, false, true));
                    done = true;
                }
            }
            if done || ui.small_button("Clear").clicked() {
                self.selection
                    .retain(|r| !(r.file == file_idx && r.hunk == hunk_idx));
                *self.anchor = None;
            }
            return;
        }
        let Some(patch) = file.hunk_patch(hunk_idx) else {
            return;
        };
        if self.staged {
            if ui.small_button("Unstage Hunk").clicked() {
                self.requests.push(("Unstage hunk", patch, true, true));
            }
        } else {
            if ui.small_button("Stage Hunk").clicked() {
                self.requests
                    .push(("Stage hunk", patch.clone(), true, false));
            }
            if ui
                .small_button("Discard Hunk")
                .on_hover_text("Revert this hunk in the working tree")
                .clicked()
            {
                self.requests.push(("Discard hunk", patch, false, true));
            }
        }
    }
}

/// One commit: message, author and its changes.
pub struct CommitPane {
    pub hash: String,
    detail: Option<Result<CommitDetail, String>>,
    task: Option<Pending<Result<CommitDetail, String>>>,
}

impl CommitPane {
    pub fn new(hash: String, sc: &SourceControl) -> Self {
        Self {
            task: sc.load_commit(&hash),
            hash,
            detail: None,
        }
    }

    fn show(&mut self, ui: &mut Ui, colors: &UiColors) -> Option<PaneAction> {
        if let Some(result) = task::poll(&mut self.task) {
            self.detail = Some(result);
        }
        let detail = match &self.detail {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(colors.error.0, e);
                return None;
            }
            None => {
                ui.spinner();
                return None;
            }
        };
        let mut action = None;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                let (subject, body) = detail
                    .message
                    .split_once('\n')
                    .unwrap_or((&detail.message, ""));
                ui.horizontal(|ui| {
                    icons::label(ui, Icon::Commit, colors.text_muted.0, 18.0);
                    ui.label(RichText::new(subject).strong().size(16.0));
                });
                if !body.trim().is_empty() {
                    ui.label(body.trim());
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&detail.author).strong());
                    ui.label(
                        RichText::new(format!("<{}>", detail.email)).color(colors.text_muted.0),
                    );
                    ui.label(
                        RichText::new(fmt::relative_time(detail.time)).color(colors.text_muted.0),
                    );
                    if ui
                        .small_button(&detail.hash[..detail.hash.len().min(10)])
                        .on_hover_text("Copy full hash")
                        .clicked()
                    {
                        ui.ctx().copy_text(detail.hash.clone());
                    }
                    let adds: usize = detail.files.iter().map(FileDiff::additions).sum();
                    let dels: usize = detail.files.iter().map(FileDiff::deletions).sum();
                    ui.label(format!("{} file(s)", detail.files.len()));
                    ui.label(RichText::new(format!("+{adds}")).color(colors.added.0));
                    ui.label(RichText::new(format!("−{dels}")).color(colors.deleted.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        diff_view::layout_toggle(ui);
                    });
                });
            });
        ui.separator();
        struct OpenHooks<'a>(&'a mut Option<PaneAction>);
        impl DiffHooks for OpenHooks<'_> {
            fn file_actions(&mut self, ui: &mut Ui, _idx: usize, file: &FileDiff) {
                if file.new_path.is_some() && ui.small_button("Open File").clicked() {
                    *self.0 = Some(PaneAction::OpenFile(file.path().to_owned()));
                }
            }
        }
        let dc = DiffColors::from_theme(colors);
        egui::ScrollArea::both()
            .id_salt(("commit", &self.hash))
            .auto_shrink(false)
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        let id = ui.make_persistent_id(("commit", &self.hash));
                        diff_view::show(
                            ui,
                            id,
                            &detail.files,
                            &dc,
                            &mut OpenHooks(&mut action),
                            None,
                        );
                    });
            });
        action
    }
}

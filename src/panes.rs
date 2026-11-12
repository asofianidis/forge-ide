//! Non-editor tabs in the main area: a file's working-tree/staged diff, a
//! commit, and a GitHub pull request.

use egui::{RichText, Ui};

use crate::git::diff::FileDiff;
use crate::git::status::Entry;
use crate::git::{CommitDetail, SourceControl};
use crate::github::PrPane;
use crate::task::{self, Pending};
use crate::theme::UiColors;
use crate::ui::diff_view::{self, DiffColors, DiffHooks};
use crate::ui::fmt;
use crate::ui::icons::{self, Icon};

/// Something a pane wants the app to do.
pub enum PaneAction {
    /// Open a repository-relative file in the editor.
    OpenFile(String),
}

pub enum Pane {
    Changes(ChangesPane),
    Commit(CommitPane),
    PullRequest(Box<PrPane>),
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
        }
    }

    pub fn icon(&self) -> Icon {
        match self {
            Pane::Changes(_) => Icon::Branch,
            Pane::Commit(_) => Icon::Commit,
            Pane::PullRequest(_) => Icon::PullRequest,
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
                p.show(ui, colors);
                None
            }
        }
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
}

impl ChangesPane {
    pub fn new(entry: Entry, staged: bool) -> Self {
        Self {
            entry,
            staged,
            files: None,
            task: None,
            loaded_rev: None,
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
        }

        let dc = DiffColors::from_theme(colors);
        let mut hooks = LocalHooks {
            staged: self.staged,
            untracked: self.entry.is_untracked(),
            requests: Vec::new(),
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

/// Stage / unstage / discard individual hunks.
struct LocalHooks {
    staged: bool,
    untracked: bool,
    /// (label, patch, cached, reverse)
    requests: Vec<(&'static str, String, bool, bool)>,
}

impl DiffHooks for LocalHooks {
    fn hunk_actions(&mut self, ui: &mut Ui, _file_idx: usize, hunk_idx: usize, file: &FileDiff) {
        if self.untracked {
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

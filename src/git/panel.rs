//! The Source Control sidebar: branch and sync controls, the commit box,
//! staged / unstaged / conflicted files, stashes, branches and history.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, Sense, Ui, UiBuilder, vec2};

use super::status::{self, Change, Entry};
use super::worktree;
use super::{Branch, RepoState, SourceControl, WorktreeHead};
use crate::task::Pending;
use crate::theme::UiColors;
use crate::ui::fmt;
use crate::ui::icons::{self, Icon};

const COMMIT_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
const HISTORY_PAGE: usize = 50;

/// What the panel wants the app to do.
#[derive(Debug, Clone)]
pub enum PanelAction {
    OpenDiff {
        entry: Entry,
        staged: bool,
    },
    OpenFile(String),
    OpenCommit(String),
    /// Switch to the Pull Requests view and start creating a PR.
    CreatePullRequest,
    /// Open the commit graph tab.
    OpenHistory,
    /// Open the interactive rebase tab for the current branch.
    OpenRebase,
    /// Open a folder (a worktree) in this window.
    OpenFolder(PathBuf),
    /// Open a folder in a new Forge window.
    OpenFolderInNewWindow(PathBuf),
    /// Show a folder in the system file manager.
    RevealPath(PathBuf),
}

/// Where to open a worktree once it has been created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenIn {
    Nowhere,
    ThisWindow,
    NewWindow,
}

/// The "New Worktree" dialog.
struct NewWorktree {
    new_branch: bool,
    branch: String,
    /// Base for a new branch; empty means HEAD.
    base: String,
    /// Existing branch to check out.
    existing: String,
    path: String,
    /// The user changed the folder, so stop deriving it from the branch.
    path_edited: bool,
    open: OpenIn,
}

/// How long to wait for a new worktree to show up before giving up on
/// opening it.
const OPEN_WORKTREE_TIMEOUT: Duration = Duration::from_secs(60);

/// Actions that need a confirmation dialog first.
#[derive(Debug, Clone)]
enum Confirm {
    Discard(Vec<Entry>),
    DeleteBranch(String),
    ForcePush,
    DropStash(String),
    DeleteTag(String),
    RemoveWorktree(PathBuf),
}

/// Deferred changes collected while drawing (status is borrowed then).
enum RowAction {
    Stage(Vec<String>),
    Unstage(Vec<String>),
    Discard(Vec<Entry>),
    Resolve(String, bool),
}

#[derive(Default)]
pub struct ScmPanel {
    message: String,
    amend: bool,
    branch_query: String,
    confirm: Option<Confirm>,
    stash_message: String,
    history_shown: usize,
    /// Name and message for a new tag (popup open when `Some`).
    new_tag: Option<(String, String)>,
    delete_tag_remote: bool,
    /// Action chosen from a menu, returned on the next `show`.
    pending_action: Option<PanelAction>,
    /// Loading HEAD's message to prefill an amend.
    amend_task: Option<crate::task::Pending<Option<String>>>,
    /// Working out a commit message from the diff.
    generate_task: Option<Pending<Result<String, String>>>,
    generate_error: Option<String>,
    new_worktree: Option<NewWorktree>,
    /// A worktree being created, to open once it appears.
    open_when_ready: Option<(PathBuf, OpenIn, Instant)>,
    remove_worktree_force: bool,
}

pub fn change_color(change: Change, colors: &UiColors) -> Color32 {
    match change {
        Change::Added | Change::Untracked => colors.added.0,
        Change::Deleted => colors.deleted.0,
        Change::Conflicted => colors.error.0,
        _ => colors.modified.0,
    }
}

impl ScmPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        sc: &mut SourceControl,
        colors: &UiColors,
    ) -> Option<PanelAction> {
        let mut action = self.pending_action.take();
        if action.is_none() {
            action = self.take_created_worktree(sc);
        }
        self.title_bar(ui, sc, colors);

        match &sc.state {
            RepoState::NoFolder => {
                ui.label(RichText::new("Open a folder to use source control.").weak());
                return None;
            }
            RepoState::Detecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for a Git repository…");
                });
                return None;
            }
            RepoState::GitMissing => {
                ui.label("Git was not found on this system.");
                ui.hyperlink_to("Download Git", "https://git-scm.com/downloads");
                return None;
            }
            RepoState::NotARepo => {
                ui.label(RichText::new("The current folder isn't a Git repository.").weak());
                ui.add_space(4.0);
                if ui.button("Initialize Repository").clicked() {
                    sc.init();
                }
                return None;
            }
            RepoState::Ready => {}
        }

        self.branch_bar(ui, sc, colors, &mut action);
        self.operation_banner(ui, sc, colors);
        ui.add_space(4.0);
        self.commit_box(ui, sc, colors);
        ui.add_space(6.0);

        let mut deferred = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt("scm_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                self.changes(ui, sc, colors, &mut action, &mut deferred);
                self.stashes(ui, sc, colors);
                self.branches(ui, sc, colors);
                self.worktrees(ui, sc, colors, &mut action);
                self.tags(ui, sc, colors);
                self.history(ui, sc, colors, &mut action);
            });
        for d in deferred {
            match d {
                RowAction::Stage(p) => sc.stage(p),
                RowAction::Unstage(p) => sc.unstage(p),
                RowAction::Discard(e) => self.confirm = Some(Confirm::Discard(e)),
                RowAction::Resolve(p, theirs) => sc.resolve_with(p, theirs),
            }
        }
        self.new_worktree_dialog(ui, sc);
        self.confirm_dialog(ui, sc);
        action
    }

    fn title_bar(&mut self, ui: &mut Ui, sc: &mut SourceControl, colors: &UiColors) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SOURCE CONTROL")
                    .small()
                    .strong()
                    .color(colors.text_muted.0),
            );
            if let Some(label) = sc.busy() {
                ui.spinner().on_hover_text(label);
            }
            if !sc.is_ready() {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let more = icons::button(ui, Icon::More, "More actions");
                egui::Popup::menu(&more).show(|ui| self.more_menu(ui, sc));
                if icons::button(ui, Icon::Refresh, "Refresh").clicked() {
                    sc.refresh();
                }
                if icons::button(ui, Icon::ArrowDown, "Fetch").clicked() {
                    sc.fetch();
                }
            });
        });
    }

    fn more_menu(&mut self, ui: &mut Ui, sc: &mut SourceControl) {
        ui.set_min_width(200.0);
        if ui.button("Pull").clicked() {
            sc.pull(false);
        }
        if ui.button("Pull (Rebase)").clicked() {
            sc.pull(true);
        }
        if ui.button("Push").clicked() {
            sc.push(false);
        }
        if ui.button("Force Push (with lease)…").clicked() {
            self.confirm = Some(Confirm::ForcePush);
        }
        if ui.button("Fetch All").clicked() {
            sc.fetch();
        }
        ui.separator();
        if ui.button("Stage All Changes").clicked() {
            sc.stage_all();
        }
        if ui.button("Unstage All Changes").clicked() {
            sc.unstage_all();
        }
        ui.separator();
        ui.menu_button("Stash", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.stash_message)
                    .hint_text("Stash message (optional)")
                    .desired_width(200.0),
            );
            if ui.button("Stash").clicked() {
                sc.stash(std::mem::take(&mut self.stash_message), false);
                ui.close();
            }
            if ui.button("Stash (Include Untracked)").clicked() {
                sc.stash(std::mem::take(&mut self.stash_message), true);
                ui.close();
            }
            if let Some(top) = sc.stashes.first().map(|s| s.name.clone())
                && ui.button("Pop Latest Stash").clicked()
            {
                sc.stash_pop(top);
                ui.close();
            }
        });
        ui.separator();
        if ui
            .button("Interactive Rebase…")
            .on_hover_text("Reorder, reword, squash or drop this branch's commits")
            .clicked()
        {
            self.pending_action = Some(PanelAction::OpenRebase);
        }
        if ui.button("Commit Graph").clicked() {
            self.pending_action = Some(PanelAction::OpenHistory);
        }
        ui.separator();
        if ui.button("New Worktree…").clicked() {
            self.start_new_worktree(sc, None);
        }
        if ui
            .button("Prune Worktrees")
            .on_hover_text("Forget worktrees whose folders were deleted")
            .clicked()
        {
            sc.prune_worktrees();
        }
        ui.separator();
        let has_commits = !sc.status.branch.is_unborn();
        if ui
            .add_enabled(has_commits, egui::Button::new("Undo Last Commit"))
            .on_hover_text("Moves HEAD back one commit and keeps its changes staged")
            .clicked()
        {
            sc.undo_last_commit();
        }
        if sc.status.conflicts().next().is_some() && ui.button("Abort Merge").clicked() {
            sc.abort_merge();
        }
    }

    fn branch_bar(
        &mut self,
        ui: &mut Ui,
        sc: &mut SourceControl,
        colors: &UiColors,
        action: &mut Option<PanelAction>,
    ) {
        let branch = sc.status.branch.clone();
        #[derive(PartialEq)]
        enum Clicked {
            Publish,
            Sync,
            CreatePr,
        }
        let mut clicked = None;
        // Right-hand buttons are laid out first; the branch name gets the
        // remaining width and is truncated so it never runs under them.
        egui::Sides::new().shrink_left().truncate().show(
            ui,
            |ui| {
                icons::label(ui, Icon::Branch, colors.text_muted.0, 16.0);
                let button = ui
                    .add(
                        // The ▾ sits in the right slot so truncating the
                        // name never hides it.
                        egui::Button::new(RichText::new(branch.display()).strong())
                            .right_text("▾")
                            .frame(false)
                            .truncate(),
                    )
                    .on_hover_text(format!("{}\nSwitch or create a branch", branch.display()));
                egui::Popup::menu(&button)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| self.branch_picker(ui, sc));
            },
            |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if branch.upstream.is_none() && branch.head.is_some() {
                    if ui
                        .add(egui::Button::new("Publish").small())
                        .on_hover_text("Push this branch and set its upstream")
                        .clicked()
                    {
                        clicked = Some(Clicked::Publish);
                    }
                } else if branch.upstream.is_some() {
                    let tip = format!(
                        "Sync with {}: pull {} and push {} commit(s)",
                        branch.upstream.as_deref().unwrap_or_default(),
                        branch.behind,
                        branch.ahead
                    );
                    if icons::button(ui, Icon::Sync, &tip).clicked() {
                        clicked = Some(Clicked::Sync);
                    }
                    if branch.ahead > 0 {
                        ui.label(RichText::new(format!("{}↑", branch.ahead)).small());
                    }
                    if branch.behind > 0 {
                        ui.label(RichText::new(format!("{}↓", branch.behind)).small());
                    }
                }
                if branch.head.is_some()
                    && icons::button(ui, Icon::PullRequest, "Create pull request for this branch")
                        .clicked()
                {
                    clicked = Some(Clicked::CreatePr);
                }
            },
        );
        match clicked {
            Some(Clicked::Publish) => sc.push(false),
            Some(Clicked::Sync) => sc.sync(),
            Some(Clicked::CreatePr) => *action = Some(PanelAction::CreatePullRequest),
            None => {}
        }
    }

    fn branch_picker(&mut self, ui: &mut Ui, sc: &mut SourceControl) {
        ui.set_min_width(280.0);
        let edit = ui.add(
            egui::TextEdit::singleline(&mut self.branch_query)
                .hint_text("Filter, or name a new branch")
                .desired_width(f32::INFINITY),
        );
        if !edit.has_focus() && self.branch_query.is_empty() {
            edit.request_focus();
        }
        let query = self.branch_query.trim().to_owned();
        let exact = sc.branches.iter().any(|b| b.name == query);
        if !query.is_empty() && !exact {
            let create = ui.button(format!("+ Create branch “{query}”"));
            if create.clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                sc.create_branch(query.clone(), None);
                self.branch_query.clear();
                ui.close();
                return;
            }
        }
        ui.separator();
        let q = query.to_lowercase();
        let matches = |b: &&Branch| q.is_empty() || b.name.to_lowercase().contains(&q);
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for (title, remote) in [("Local", false), ("Remote", true)] {
                    let list: Vec<&Branch> = sc
                        .branches
                        .iter()
                        .filter(|b| b.remote == remote)
                        .filter(matches)
                        .collect();
                    if list.is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(title).small().weak());
                    for b in list {
                        let text = if b.current {
                            RichText::new(format!("✔ {}", b.name)).strong()
                        } else {
                            RichText::new(format!("   {}", b.name))
                        };
                        let resp = ui
                            .add(egui::Button::new(text).frame(false).right_text(
                                RichText::new(fmt::relative_time(b.time)).small().weak(),
                            ))
                            .on_hover_text(&b.subject);
                        if resp.clicked() && !b.current {
                            chosen = Some(b.clone());
                        }
                    }
                }
            });
        if let Some(b) = chosen {
            sc.checkout(&b);
            self.branch_query.clear();
            ui.close();
        }
    }

    fn commit_box(&mut self, ui: &mut Ui, sc: &mut SourceControl, colors: &UiColors) {
        let id = ui.make_persistent_id("commit_message");
        let focused = ui.memory(|m| m.has_focus(id));
        let shortcut = focused && ui.input_mut(|i| i.consume_shortcut(&COMMIT_SHORTCUT));

        let branch = sc.status.branch.display();
        let hint = format!(
            "Message ({} to commit on “{branch}”)",
            ui.ctx().format_shortcut(&COMMIT_SHORTCUT)
        );
        if let Some(Some(previous)) = crate::task::poll(&mut self.amend_task)
            && self.message.trim().is_empty()
        {
            self.message = previous;
        }
        ui.add(
            egui::TextEdit::multiline(&mut self.message)
                .id(id)
                .hint_text(hint)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        if let Some(result) = crate::task::poll(&mut self.generate_task) {
            match result {
                Ok(message) => {
                    self.message = message;
                    self.generate_error = None;
                }
                Err(e) => self.generate_error = Some(e),
            }
        }
        egui::Sides::new().shrink_left().show(
            ui,
            |ui| message_guide(ui, &self.message, colors),
            |ui| {
                if self.generate_task.is_some() {
                    ui.spinner();
                    return;
                }
                let has_changes = !sc.status.entries.is_empty();
                let what = if sc.status.staged().next().is_some() {
                    "the staged changes"
                } else {
                    "all changes"
                };
                let tip = if self.message.trim().is_empty() {
                    format!("Write a message describing {what}")
                } else {
                    format!("Replace the message with one describing {what}")
                };
                if ui
                    .add_enabled(
                        has_changes,
                        egui::Button::new(RichText::new("Generate").small()),
                    )
                    .on_hover_text(tip)
                    .on_disabled_hover_text("There are no changes to describe")
                    .clicked()
                {
                    self.generate_error = None;
                    self.generate_task = sc.suggest_commit_message();
                }
            },
        );
        if let Some(e) = &self.generate_error {
            ui.label(RichText::new(e).small().color(colors.error.0));
        }

        let staged = sc.status.staged().count();
        let changed = sc.status.entries.len();
        let conflicts = sc.status.conflicts().count();
        let has_message = !self.message.trim().is_empty();
        let can_commit =
            conflicts == 0 && sc.busy().is_none() && (self.amend || (has_message && changed > 0));
        let label = match (self.amend, staged) {
            (true, _) => "Amend Commit".to_owned(),
            (false, 0) if changed > 0 => "Commit All".to_owned(),
            _ => "Commit".to_owned(),
        };
        let mut commit = shortcut && can_commit;
        let mut push_after = false;
        ui.horizontal(|ui| {
            // Leave exact room for the ▾ button; overshooting would make the
            // resizable sidebar grow a little every frame.
            let width = ui.available_width() - 24.0 - ui.spacing().item_spacing.x;
            let button = egui::Button::new(RichText::new(&label).color(colors.accent_text.0))
                .fill(colors.accent.0)
                .min_size(vec2(width, 26.0));
            let tip = if conflicts > 0 {
                "Resolve merge conflicts first"
            } else if !has_message && !self.amend {
                "Write a commit message"
            } else if staged == 0 && !self.amend {
                "Nothing is staged, so all changes will be committed"
            } else {
                "Commit staged changes"
            };
            if ui
                .add_enabled(can_commit, button)
                .on_hover_text(tip)
                .on_disabled_hover_text(tip)
                .clicked()
            {
                commit = true;
            }
            let more = ui.add(egui::Button::new("▾").min_size(vec2(24.0, 26.0)));
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(180.0);
                if ui
                    .add_enabled(can_commit, egui::Button::new("Commit & Push"))
                    .clicked()
                {
                    commit = true;
                    push_after = true;
                }
                if ui
                    .checkbox(&mut self.amend, "Amend previous commit")
                    .changed()
                    && self.amend
                    && self.message.trim().is_empty()
                {
                    // Start from the previous message, like `git commit --amend`.
                    self.amend_task = sc.load_last_message();
                }
            });
        });
        if self.amend {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Amending")
                        .small()
                        .strong()
                        .color(colors.warning.0),
                );
                if let Some(last) = sc.log.first() {
                    ui.label(
                        RichText::new(format!("{} {}", last.short, last.subject))
                            .small()
                            .color(colors.text_muted.0),
                    );
                }
                let unpushed = sc.status.branch.ahead > 0 || sc.status.branch.upstream.is_none();
                if !unpushed {
                    ui.label(
                        RichText::new("— already pushed; you'll need to force push")
                            .small()
                            .color(colors.warning.0),
                    );
                }
            });
            let staged: Vec<&str> = sc.status.staged().map(|e| e.path.as_str()).collect();
            if !staged.is_empty() {
                ui.label(
                    RichText::new(format!("Adds {} staged file(s) to it", staged.len()))
                        .small()
                        .color(colors.text_muted.0),
                )
                .on_hover_text(staged.join("\n"));
            }
        }
        if commit {
            sc.commit(std::mem::take(&mut self.message), self.amend, push_after);
            self.amend = false;
        }
    }

    fn changes(
        &mut self,
        ui: &mut Ui,
        sc: &SourceControl,
        colors: &UiColors,
        action: &mut Option<PanelAction>,
        deferred: &mut Vec<RowAction>,
    ) {
        let status = &sc.status;
        let conflicts: Vec<&Entry> = status.conflicts().collect();
        let staged: Vec<&Entry> = status.staged().collect();
        let unstaged: Vec<&Entry> = status.unstaged().collect();

        if !conflicts.is_empty() {
            section(
                ui,
                "Merge Changes",
                conflicts.len(),
                true,
                colors,
                |_| {},
                |ui| {
                    for e in &conflicts {
                        file_row(ui, e, RowKind::Conflict, colors, action, deferred);
                    }
                },
            );
        }
        let mut header_actions = Vec::new();
        if !staged.is_empty() {
            section(
                ui,
                "Staged Changes",
                staged.len(),
                true,
                colors,
                |ui| {
                    if icons::button(ui, Icon::Minus, "Unstage all").clicked() {
                        header_actions.push(RowAction::Unstage(
                            staged.iter().map(|e| e.path.clone()).collect(),
                        ));
                    }
                },
                |ui| {
                    for e in &staged {
                        file_row(ui, e, RowKind::Staged, colors, action, deferred);
                    }
                },
            );
        }
        section(
            ui,
            "Changes",
            unstaged.len(),
            true,
            colors,
            |ui| {
                if unstaged.is_empty() {
                    return;
                }
                if icons::button(ui, Icon::Plus, "Stage all changes").clicked() {
                    header_actions.push(RowAction::Stage(
                        unstaged.iter().map(|e| e.path.clone()).collect(),
                    ));
                }
                if icons::button(ui, Icon::Discard, "Discard all changes").clicked() {
                    header_actions.push(RowAction::Discard(
                        unstaged.iter().map(|e| (*e).clone()).collect(),
                    ));
                }
            },
            |ui| {
                if unstaged.is_empty() && staged.is_empty() && conflicts.is_empty() {
                    ui.label(RichText::new("No changes. Working tree clean.").weak());
                }
                for e in &unstaged {
                    file_row(ui, e, RowKind::Unstaged, colors, action, deferred);
                }
            },
        );
        deferred.extend(header_actions);
    }

    fn stashes(&mut self, ui: &mut Ui, sc: &mut SourceControl, colors: &UiColors) {
        if sc.stashes.is_empty() {
            return;
        }
        let mut todo: Option<(u8, String)> = None;
        section(
            ui,
            "Stashes",
            sc.stashes.len(),
            false,
            colors,
            |_| {},
            |ui| {
                for s in &sc.stashes {
                    hover_row(ui, 22.0, |ui, hovered| {
                        icons::label(ui, Icon::Stash, colors.text_muted.0, 14.0);
                        ui.label(&s.message).on_hover_text(format!(
                            "{} · {}",
                            s.name,
                            fmt::relative_time(s.time)
                        ));
                        if hovered {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    if icons::button(ui, Icon::Close, "Drop stash").clicked() {
                                        todo = Some((2, s.name.clone()));
                                    }
                                    if icons::button(
                                        ui,
                                        Icon::ArrowUp,
                                        "Pop stash (apply and remove)",
                                    )
                                    .clicked()
                                    {
                                        todo = Some((1, s.name.clone()));
                                    }
                                    if icons::button(ui, Icon::Check, "Apply stash (keep it)")
                                        .clicked()
                                    {
                                        todo = Some((0, s.name.clone()));
                                    }
                                },
                            );
                        }
                    });
                }
            },
        );
        match todo {
            Some((0, n)) => sc.stash_apply(n),
            Some((1, n)) => sc.stash_pop(n),
            Some((_, n)) => self.confirm = Some(Confirm::DropStash(n)),
            None => {}
        }
    }

    fn branches(&mut self, ui: &mut Ui, sc: &mut SourceControl, colors: &UiColors) {
        let local: Vec<Branch> = sc.branches.iter().filter(|b| !b.remote).cloned().collect();
        if local.is_empty() {
            return;
        }
        let mut checkout = None;
        let mut merge = None;
        section(
            ui,
            "Branches",
            local.len(),
            false,
            colors,
            |_| {},
            |ui| {
                for b in &local {
                    hover_row(ui, 22.0, |ui, hovered| {
                        let name = if b.current {
                            RichText::new(&b.name).strong()
                        } else {
                            RichText::new(&b.name)
                        };
                        icons::label(
                            ui,
                            Icon::Branch,
                            if b.current {
                                colors.accent.0
                            } else {
                                colors.text_muted.0
                            },
                            14.0,
                        );
                        ui.label(name).on_hover_text(format!(
                            "{}\n{}{}",
                            b.subject,
                            b.upstream
                                .as_deref()
                                .map(|u| format!("tracks {u} "))
                                .unwrap_or_default(),
                            b.track
                        ));
                        if !b.track.is_empty() {
                            ui.label(RichText::new(&b.track).small().weak());
                        }
                        let elsewhere = sc
                            .worktrees
                            .iter()
                            .find(|w| !w.current && w.branch.as_deref() == Some(b.name.as_str()));
                        if let Some(w) = elsewhere {
                            ui.label(RichText::new("worktree").small().color(colors.link.0))
                                .on_hover_text(format!("Checked out in {}", w.path.display()));
                        }
                        if hovered && !b.current {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    if icons::button(ui, Icon::Close, "Delete branch").clicked() {
                                        self.confirm = Some(Confirm::DeleteBranch(b.name.clone()));
                                    }
                                    if icons::button(ui, Icon::Merge, "Merge into current branch")
                                        .clicked()
                                    {
                                        merge = Some(b.name.clone());
                                    }
                                    if icons::button(ui, Icon::Check, "Check out").clicked() {
                                        checkout = Some(b.clone());
                                    }
                                },
                            );
                        }
                    });
                }
            },
        );
        if let Some(b) = checkout {
            sc.checkout(&b);
        }
        if let Some(n) = merge {
            sc.merge_branch(n);
        }
    }

    fn worktrees(
        &mut self,
        ui: &mut Ui,
        sc: &mut SourceControl,
        colors: &UiColors,
        action: &mut Option<PanelAction>,
    ) {
        if sc.worktrees.is_empty() {
            return;
        }
        let mut create = false;
        let mut chosen: Option<PanelAction> = None;
        let mut remove = None;
        let mut lock = None;
        let mut prune = false;
        section(
            ui,
            "Worktrees",
            sc.worktrees.len(),
            false,
            colors,
            |ui| {
                if icons::button(ui, Icon::Plus, "New worktree").clicked() {
                    create = true;
                }
            },
            |ui| {
                if sc.worktrees.len() == 1 {
                    ui.label(
                        RichText::new(
                            "Worktrees let you check out another branch in its own folder.",
                        )
                        .small()
                        .weak(),
                    );
                }
                for w in &sc.worktrees {
                    let resp = hover_row(ui, 36.0, |ui, hovered| {
                        icons::label(
                            ui,
                            Icon::Files,
                            if w.current {
                                colors.accent.0
                            } else {
                                colors.text_muted.0
                            },
                            14.0,
                        );
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.horizontal(|ui| {
                                let name = RichText::new(w.name());
                                ui.add(
                                    egui::Label::new(if w.current { name.strong() } else { name })
                                        .truncate(),
                                );
                                if w.current {
                                    ui.label(RichText::new("open").small().color(colors.accent.0));
                                } else if w.main {
                                    ui.label(RichText::new("main").small().weak());
                                }
                            });
                            let mut detail = vec![w.describe_head()];
                            if w.locked.is_some() {
                                detail.push("locked".to_owned());
                            }
                            if w.prunable.is_some() {
                                detail.push("missing".to_owned());
                            }
                            ui.label(RichText::new(detail.join(" · ")).small().color(
                                if w.prunable.is_some() {
                                    colors.warning.0
                                } else {
                                    colors.text_muted.0
                                },
                            ));
                        });
                        if hovered && !w.current && w.prunable.is_none() {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    if !w.main
                                        && icons::button(ui, Icon::Close, "Remove worktree…")
                                            .clicked()
                                    {
                                        remove = Some(w.path.clone());
                                    }
                                    if icons::button(ui, Icon::External, "Open in new window")
                                        .clicked()
                                    {
                                        chosen = Some(PanelAction::OpenFolderInNewWindow(
                                            w.path.clone(),
                                        ));
                                    }
                                },
                            );
                        }
                    });
                    if resp.clicked() && !w.current && w.prunable.is_none() {
                        chosen = Some(PanelAction::OpenFolder(w.path.clone()));
                    }
                    let mut tip = w.path.display().to_string();
                    if let Some(reason) = w.locked.as_deref().filter(|r| !r.is_empty()) {
                        tip.push_str(&format!("\nLocked: {reason}"));
                    }
                    if let Some(reason) = &w.prunable {
                        tip.push_str(&format!("\nMissing: {reason}"));
                    }
                    if !w.current {
                        tip.push_str("\nClick to open in this window");
                    }
                    let resp = resp.on_hover_text(tip);
                    resp.context_menu(|ui| {
                        let exists = w.prunable.is_none();
                        if ui
                            .add_enabled(exists && !w.current, egui::Button::new("Open"))
                            .clicked()
                        {
                            chosen = Some(PanelAction::OpenFolder(w.path.clone()));
                        }
                        if ui
                            .add_enabled(exists, egui::Button::new("Open in New Window"))
                            .clicked()
                        {
                            chosen = Some(PanelAction::OpenFolderInNewWindow(w.path.clone()));
                        }
                        if ui
                            .add_enabled(exists, egui::Button::new(reveal_label()))
                            .clicked()
                        {
                            chosen = Some(PanelAction::RevealPath(w.path.clone()));
                        }
                        if ui.button("Copy Path").clicked() {
                            ui.ctx().copy_text(w.path.display().to_string());
                        }
                        if !w.main {
                            ui.separator();
                            let locked = w.locked.is_some();
                            if ui
                                .button(if locked { "Unlock" } else { "Lock" })
                                .on_hover_text("A locked worktree can't be removed or pruned")
                                .clicked()
                            {
                                lock = Some((w.path.clone(), !locked));
                            }
                            if w.prunable.is_some() {
                                if ui.button("Prune Missing Worktrees").clicked() {
                                    prune = true;
                                }
                            } else if ui
                                .add_enabled(!w.current, egui::Button::new("Remove…"))
                                .on_disabled_hover_text("Open another worktree first")
                                .clicked()
                            {
                                remove = Some(w.path.clone());
                            }
                        }
                    });
                }
            },
        );
        if create {
            self.start_new_worktree(sc, None);
        }
        if let Some(path) = remove {
            self.remove_worktree_force = false;
            self.confirm = Some(Confirm::RemoveWorktree(path));
        }
        if let Some((path, l)) = lock {
            sc.lock_worktree(path, l);
        }
        if prune {
            sc.prune_worktrees();
        }
        if chosen.is_some() {
            *action = chosen;
        }
    }

    /// Opens the "New Worktree" dialog, optionally for an existing branch.
    fn start_new_worktree(&mut self, sc: &SourceControl, existing: Option<String>) {
        let base = sc.status.branch.head.clone().unwrap_or_default();
        self.new_worktree = Some(NewWorktree {
            new_branch: existing.is_none(),
            branch: String::new(),
            base,
            existing: existing.unwrap_or_default(),
            path: String::new(),
            path_edited: false,
            open: OpenIn::NewWindow,
        });
    }

    fn new_worktree_dialog(&mut self, ui: &mut Ui, sc: &mut SourceControl) {
        let Some(form) = &mut self.new_worktree else {
            return;
        };
        let main = sc
            .worktrees
            .first()
            .map(|w| w.path.clone())
            .or_else(|| sc.root().map(Path::to_path_buf));
        // Branches that can be checked out: not already in a worktree.
        let taken: Vec<&str> = sc
            .worktrees
            .iter()
            .filter_map(|w| w.branch.as_deref())
            .collect();
        let available: Vec<&Branch> = sc
            .branches
            .iter()
            .filter(|b| !b.name.ends_with("/HEAD"))
            .filter(|b| b.remote || !taken.contains(&b.name.as_str()))
            .collect();

        let mut done = None;
        egui::Modal::new(egui::Id::unique("new_worktree")).show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            ui.heading("New Worktree");
            ui.label(
                RichText::new(
                    "Check out a branch in a separate folder, so you can work on it \
                               without stashing or switching here.",
                )
                .small()
                .weak(),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.radio_value(&mut form.new_branch, true, "New branch");
                ui.radio_value(&mut form.new_branch, false, "Existing branch");
            });
            egui::Grid::new("new_worktree_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    if form.new_branch {
                        ui.label("Branch");
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut form.branch)
                                .hint_text("feature/my-change")
                                .desired_width(f32::INFINITY),
                        );
                        if form.branch.is_empty() && !ui.memory(|m| m.focused().is_some()) {
                            field.request_focus();
                        }
                        ui.end_row();
                        ui.label("From");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.base)
                                .hint_text("HEAD")
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();
                    } else {
                        ui.label("Branch");
                        let shown = if form.existing.is_empty() {
                            "Choose a branch…".to_owned()
                        } else {
                            form.existing.clone()
                        };
                        egui::ComboBox::from_id_salt("worktree_existing")
                            .selected_text(shown)
                            .width(ui.available_width())
                            .height(300.0)
                            .show_ui(ui, |ui| {
                                for b in &available {
                                    ui.selectable_value(
                                        &mut form.existing,
                                        b.name.clone(),
                                        &b.name,
                                    );
                                }
                            });
                        ui.end_row();
                    }
                    let branch = if form.new_branch {
                        form.branch.trim().to_owned()
                    } else {
                        // `origin/feature` checks out as `feature`.
                        let name = form.existing.as_str();
                        sc.branches
                            .iter()
                            .find(|b| b.remote && b.name == name)
                            .and_then(|_| name.split_once('/'))
                            .map_or(name, |(_, local)| local)
                            .to_owned()
                    };
                    if !form.path_edited
                        && let Some(main) = &main
                    {
                        form.path = worktree::default_path(main, &branch)
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                    }
                    ui.label("Folder");
                    ui.horizontal(|ui| {
                        let browse_width = 70.0;
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut form.path)
                                    .desired_width(ui.available_width() - browse_width),
                            )
                            .changed()
                        {
                            form.path_edited = true;
                        }
                        if ui.button("Browse…").clicked() {
                            let mut dialog = rfd::FileDialog::new()
                                .set_title("Choose where to put the worktree folder");
                            if let Some(parent) = main.as_deref().and_then(Path::parent) {
                                dialog = dialog.set_directory(parent);
                            }
                            if let Some(dir) = dialog.pick_folder() {
                                let slug = worktree::folder_slug(&branch);
                                let path = if slug.is_empty() { dir } else { dir.join(slug) };
                                form.path = path.display().to_string();
                                form.path_edited = true;
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Then");
                    egui::ComboBox::from_id_salt("worktree_open")
                        .selected_text(match form.open {
                            OpenIn::NewWindow => "Open in a new window",
                            OpenIn::ThisWindow => "Open in this window",
                            OpenIn::Nowhere => "Don't open it",
                        })
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut form.open,
                                OpenIn::NewWindow,
                                "Open in a new window",
                            );
                            ui.selectable_value(
                                &mut form.open,
                                OpenIn::ThisWindow,
                                "Open in this window",
                            );
                            ui.selectable_value(&mut form.open, OpenIn::Nowhere, "Don't open it");
                        });
                    ui.end_row();
                });

            let problem = new_worktree_problem(form, sc);
            if let Some(p) = &problem {
                ui.label(RichText::new(p).small().color(ui.visuals().warn_fg_color));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        problem.is_none() && sc.busy().is_none(),
                        egui::Button::new("Create"),
                    )
                    .clicked()
                    || (problem.is_none() && ui.input(|i| i.key_pressed(Key::Enter)))
                {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                let form = self.new_worktree.take().expect("dialog is open");
                let path = PathBuf::from(form.path.trim());
                let head = if form.new_branch {
                    let base = form.base.trim();
                    WorktreeHead::NewBranch {
                        name: form.branch.trim().to_owned(),
                        base: (!base.is_empty()).then(|| base.to_owned()),
                    }
                } else {
                    WorktreeHead::Existing(form.existing)
                };
                sc.add_worktree(path.clone(), head);
                if form.open != OpenIn::Nowhere {
                    self.open_when_ready = Some((path, form.open, Instant::now()));
                }
            }
            Some(false) => self.new_worktree = None,
            None => {}
        }
    }

    /// Opens a just-created worktree once git reports it.
    fn take_created_worktree(&mut self, sc: &SourceControl) -> Option<PanelAction> {
        let (path, how, started) = self.open_when_ready.as_ref()?;
        if started.elapsed() > OPEN_WORKTREE_TIMEOUT {
            self.open_when_ready = None;
            return None;
        }
        sc.request_repaint_after(Duration::from_millis(250));
        let created = sc.worktrees.iter().find(|w| same_path(&w.path, path))?;
        let path = created.path.clone();
        let action = match how {
            OpenIn::ThisWindow => PanelAction::OpenFolder(path),
            OpenIn::NewWindow => PanelAction::OpenFolderInNewWindow(path),
            OpenIn::Nowhere => return None,
        };
        self.open_when_ready = None;
        Some(action)
    }

    fn history(
        &mut self,
        ui: &mut Ui,
        sc: &SourceControl,
        colors: &UiColors,
        action: &mut Option<PanelAction>,
    ) {
        if sc.log.is_empty() {
            return;
        }
        if self.history_shown == 0 {
            self.history_shown = HISTORY_PAGE;
        }
        let mut open_graph = false;
        section(
            ui,
            "History",
            sc.log.len(),
            false,
            colors,
            |ui| {
                if icons::button(ui, Icon::History, "Open the commit graph").clicked() {
                    open_graph = true;
                }
            },
            |ui| {
                for c in sc.log.iter().take(self.history_shown) {
                    let height = if c.refs.is_empty() { 36.0 } else { 50.0 };
                    let resp = hover_row(ui, height, |ui, _| {
                        icons::label(ui, Icon::Commit, colors.text_muted.0, 14.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add(egui::Label::new(&c.subject).truncate());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} · {}",
                                        c.short,
                                        c.author,
                                        fmt::relative_time(c.time)
                                    ))
                                    .small()
                                    .color(colors.text_muted.0),
                                );
                            });
                            if !c.refs.is_empty() {
                                ui.label(RichText::new(&c.refs).small().color(colors.link.0));
                            }
                        });
                    });
                    if resp.clicked() {
                        *action = Some(PanelAction::OpenCommit(c.hash.clone()));
                    }
                    resp.on_hover_text(format!("{}\n{} <{}>", c.hash, c.author, c.email));
                }
                if sc.log.len() > self.history_shown && ui.small_button("Show more").clicked() {
                    self.history_shown += HISTORY_PAGE;
                }
            },
        );
        if open_graph {
            *action = Some(PanelAction::OpenHistory);
        }
    }

    /// Shown while a rebase, merge, cherry-pick or revert is paused.
    fn operation_banner(&mut self, ui: &mut Ui, sc: &mut SourceControl, colors: &UiColors) {
        let Some(op) = sc.status.operation else {
            return;
        };
        let conflicts = sc.status.conflicts().count();
        egui::Frame::new()
            .fill(colors.warning.0.gamma_multiply(0.14))
            .corner_radius(4)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(format!("{} in progress", op.describe()))
                        .strong()
                        .color(colors.warning.0),
                );
                ui.label(
                    RichText::new(if conflicts > 0 {
                        format!(
                            "Resolve {conflicts} conflicted file(s), stage them, then continue."
                        )
                    } else {
                        "Stage your changes, then continue.".to_owned()
                    })
                    .small(),
                );
                ui.horizontal(|ui| {
                    let busy = sc.busy().is_some();
                    if ui
                        .add_enabled(conflicts == 0 && !busy, egui::Button::new("Continue"))
                        .on_disabled_hover_text("Resolve conflicts first")
                        .clicked()
                    {
                        sc.continue_operation(op);
                    }
                    if matches!(
                        op,
                        status::Operation::Rebase | status::Operation::CherryPick
                    ) && ui
                        .add_enabled(!busy, egui::Button::new("Skip"))
                        .on_hover_text("Leave out the current commit")
                        .clicked()
                    {
                        sc.skip_operation(op);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Abort"))
                        .on_hover_text("Go back to how things were before")
                        .clicked()
                    {
                        sc.abort_operation(op);
                    }
                });
            });
    }

    fn tags(&mut self, ui: &mut Ui, sc: &mut SourceControl, colors: &UiColors) {
        let mut create = false;
        let mut push_all = false;
        let mut todo: Option<(bool, String)> = None;
        section(
            ui,
            "Tags",
            sc.tags.len(),
            false,
            colors,
            |ui| {
                if icons::button(ui, Icon::ArrowUp, "Push all tags").clicked() {
                    push_all = true;
                }
                if icons::button(ui, Icon::Plus, "Create a tag at HEAD").clicked() {
                    create = true;
                }
            },
            |ui| {
                if sc.tags.is_empty() {
                    ui.label(RichText::new("No tags yet.").weak());
                }
                for t in &sc.tags {
                    hover_row(ui, 22.0, |ui, hovered| {
                        ui.label(RichText::new(&t.name).strong());
                        ui.label(
                            RichText::new(format!("{} · {}", t.target, fmt::relative_time(t.time)))
                                .small()
                                .color(colors.text_muted.0),
                        )
                        .on_hover_text(&t.subject);
                        if hovered {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    if icons::button(ui, Icon::Close, "Delete tag").clicked() {
                                        todo = Some((false, t.name.clone()));
                                    }
                                    if icons::button(ui, Icon::ArrowUp, "Push tag").clicked() {
                                        todo = Some((true, t.name.clone()));
                                    }
                                },
                            );
                        }
                    });
                }
            },
        );
        if push_all {
            sc.push_tag(None);
        }
        if create {
            self.new_tag = Some((String::new(), String::new()));
        }
        match todo {
            Some((true, name)) => sc.push_tag(Some(name)),
            Some((false, name)) => {
                self.delete_tag_remote = false;
                self.confirm = Some(Confirm::DeleteTag(name));
            }
            None => {}
        }
        self.new_tag_dialog(ui, sc);
    }

    fn new_tag_dialog(&mut self, ui: &mut Ui, sc: &mut SourceControl) {
        let Some((name, message)) = &mut self.new_tag else {
            return;
        };
        let mut done = None;
        egui::Modal::new(egui::Id::unique("new_tag")).show(ui.ctx(), |ui| {
            ui.set_width(320.0);
            ui.heading("Create Tag");
            ui.label(RichText::new("At the current commit (HEAD)").weak());
            let field = ui.add(
                egui::TextEdit::singleline(name)
                    .hint_text("v1.2.3")
                    .desired_width(f32::INFINITY),
            );
            // Focus the name once, when the dialog opens.
            if name.is_empty() && message.is_empty() && !ui.memory(|m| m.focused().is_some()) {
                field.request_focus();
            }
            ui.add(
                egui::TextEdit::multiline(message)
                    .hint_text("Message (optional; makes an annotated tag)")
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                let ok = !name.trim().is_empty() && !name.contains(char::is_whitespace);
                if ui.add_enabled(ok, egui::Button::new("Create")).clicked() {
                    done = Some(1);
                }
                if ui
                    .add_enabled(ok, egui::Button::new("Create & Push"))
                    .clicked()
                {
                    done = Some(2);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    done = Some(0);
                }
            });
        });
        if let Some(choice) = done {
            if let Some((name, message)) = self.new_tag.take()
                && choice > 0
            {
                let name = name.trim().to_owned();
                sc.create_tag(name.clone(), message, None);
                if choice == 2 {
                    sc.push_tag(Some(name));
                }
            }
            self.new_tag = None;
        }
    }

    fn confirm_dialog(&mut self, ui: &mut Ui, sc: &mut SourceControl) {
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let (title, body, button) = match &confirm {
            Confirm::Discard(entries) => {
                let untracked = entries.iter().filter(|e| e.is_untracked()).count();
                let what = match entries.as_slice() {
                    [one] => format!("“{}”", one.path),
                    _ => format!("{} files", entries.len()),
                };
                let mut body = format!("Discard changes in {what}? This can't be undone.");
                if untracked > 0 {
                    body.push_str(&format!("\n{untracked} untracked file(s) will be deleted."));
                }
                ("Discard Changes", body, "Discard")
            }
            Confirm::DeleteBranch(name) => (
                "Delete Branch",
                format!("Delete the branch “{name}”?"),
                "Delete",
            ),
            Confirm::ForcePush => (
                "Force Push",
                "Overwrite the remote branch with your local history? Uses --force-with-lease, \
                 so it fails if someone else pushed in the meantime."
                    .to_owned(),
                "Force Push",
            ),
            Confirm::DropStash(name) => {
                ("Drop Stash", format!("Permanently delete {name}?"), "Drop")
            }
            Confirm::RemoveWorktree(path) => (
                "Remove Worktree",
                format!(
                    "Remove the worktree at “{}”? Its folder is deleted; the branch is kept.",
                    path.display()
                ),
                "Remove",
            ),
            Confirm::DeleteTag(name) => (
                "Delete Tag",
                format!(
                    "Delete the tag “{name}” locally{}?",
                    if self.delete_tag_remote {
                        " and on the remote"
                    } else {
                        ""
                    }
                ),
                "Delete",
            ),
        };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("scm_confirm")).show(ui.ctx(), |ui| {
            ui.set_max_width(360.0);
            ui.heading(title);
            ui.add_space(4.0);
            ui.label(body);
            if let Confirm::DeleteTag(_) = &confirm {
                ui.checkbox(&mut self.delete_tag_remote, "Also delete it on the remote");
            }
            if let Confirm::RemoveWorktree(_) = &confirm {
                ui.checkbox(
                    &mut self.remove_worktree_force,
                    "Even if it has uncommitted changes (they will be lost)",
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(button).color(ui.visuals().error_fg_color))
                    .clicked()
                {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                match confirm {
                    Confirm::Discard(entries) => sc.discard(entries),
                    Confirm::DeleteBranch(name) => sc.delete_branch(name, false),
                    Confirm::ForcePush => sc.push(true),
                    Confirm::DropStash(name) => sc.stash_drop(name),
                    Confirm::RemoveWorktree(path) => {
                        sc.remove_worktree(path, self.remove_worktree_force)
                    }
                    Confirm::DeleteTag(name) => {
                        if self.delete_tag_remote {
                            sc.delete_remote_tag(name.clone());
                        }
                        sc.delete_tag(name);
                    }
                }
                self.confirm = None;
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Staged,
    Unstaged,
    Conflict,
}

/// A collapsible section with a title, count and header buttons.
fn section(
    ui: &mut Ui,
    title: &str,
    count: usize,
    default_open: bool,
    colors: &UiColors,
    header_buttons: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui),
) {
    let id = ui.make_persistent_id(("scm_section", title));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open)
        .show_header(ui, |ui| {
            ui.label(RichText::new(title).strong().small());
            ui.label(
                RichText::new(count.to_string())
                    .small()
                    .color(colors.text_muted.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                header_buttons(ui);
            });
        })
        .body(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            body(ui);
        });
}

/// A full-width clickable row of fixed `height`; `add` gets whether the
/// row is hovered so it can reveal its action buttons. Widgets added by
/// `add` sit on top of the row, so their clicks don't count as row clicks.
fn hover_row(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui, bool)) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = ui.rect_contains_pointer(rect);
    if hovered {
        ui.painter().rect_filled(
            rect,
            3,
            ui.visuals()
                .widgets
                .hovered
                .weak_bg_fill
                .gamma_multiply(0.5),
        );
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(2.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    add(&mut child, hovered);
    response
}

fn file_row(
    ui: &mut Ui,
    entry: &Entry,
    kind: RowKind,
    colors: &UiColors,
    action: &mut Option<PanelAction>,
    deferred: &mut Vec<RowAction>,
) {
    let change = match kind {
        RowKind::Staged => entry.staged.unwrap_or(Change::Modified),
        RowKind::Unstaged => entry.unstaged.unwrap_or(Change::Modified),
        RowKind::Conflict => Change::Conflicted,
    };
    let color = change_color(change, colors);
    let response = hover_row(ui, 22.0, |ui, hovered| {
        ui.add_space(8.0);
        let name = RichText::new(entry.file_name()).color(if change == Change::Deleted {
            colors.text_muted.0
        } else {
            colors.text.0
        });
        let name = if change == Change::Deleted {
            name.strikethrough()
        } else {
            name
        };
        ui.add(egui::Label::new(name).truncate().selectable(false));
        if !entry.dir().is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(entry.dir())
                        .small()
                        .color(colors.text_muted.0),
                )
                .truncate()
                .selectable(false),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.label(
                RichText::new(change.letter())
                    .monospace()
                    .strong()
                    .color(color),
            )
            .on_hover_text(match entry.conflict {
                Some(c) => format!("Conflict: {}", c.describe()),
                None => change.describe().to_owned(),
            });
            if !hovered {
                return;
            }
            let paths = vec![entry.path.clone()];
            match kind {
                RowKind::Staged => {
                    if icons::button(ui, Icon::Minus, "Unstage").clicked() {
                        deferred.push(RowAction::Unstage(paths));
                    }
                }
                RowKind::Unstaged => {
                    if icons::button(ui, Icon::Plus, "Stage").clicked() {
                        deferred.push(RowAction::Stage(paths));
                    }
                    if icons::button(ui, Icon::Discard, "Discard changes").clicked() {
                        deferred.push(RowAction::Discard(vec![entry.clone()]));
                    }
                }
                RowKind::Conflict => {
                    if icons::button(ui, Icon::Plus, "Mark as resolved (stage)").clicked() {
                        deferred.push(RowAction::Stage(paths));
                    }
                }
            }
            if change != Change::Deleted && icons::button(ui, Icon::OpenFile, "Open file").clicked()
            {
                *action = Some(PanelAction::OpenFile(entry.path.clone()));
            }
        });
    });
    if response.clicked() && action.is_none() && deferred.is_empty() {
        *action = Some(if kind == RowKind::Conflict {
            PanelAction::OpenFile(entry.path.clone())
        } else {
            PanelAction::OpenDiff {
                entry: entry.clone(),
                staged: kind == RowKind::Staged,
            }
        });
    }
    let tip = match &entry.orig_path {
        Some(orig) => format!("{} (renamed from {orig})", entry.path),
        None => entry.path.clone(),
    };
    response.on_hover_text(tip).context_menu(|ui| {
        if ui.button("Open Changes").clicked() && kind != RowKind::Conflict {
            *action = Some(PanelAction::OpenDiff {
                entry: entry.clone(),
                staged: kind == RowKind::Staged,
            });
            ui.close();
        }
        if change != Change::Deleted && ui.button("Open File").clicked() {
            *action = Some(PanelAction::OpenFile(entry.path.clone()));
            ui.close();
        }
        ui.separator();
        match kind {
            RowKind::Staged => {
                if ui.button("Unstage").clicked() {
                    deferred.push(RowAction::Unstage(vec![entry.path.clone()]));
                    ui.close();
                }
            }
            RowKind::Unstaged => {
                if ui.button("Stage").clicked() {
                    deferred.push(RowAction::Stage(vec![entry.path.clone()]));
                    ui.close();
                }
                if ui.button("Discard Changes").clicked() {
                    deferred.push(RowAction::Discard(vec![entry.clone()]));
                    ui.close();
                }
            }
            RowKind::Conflict => {
                if ui.button("Accept Current (Ours)").clicked() {
                    deferred.push(RowAction::Resolve(entry.path.clone(), false));
                    ui.close();
                }
                if ui.button("Accept Incoming (Theirs)").clicked() {
                    deferred.push(RowAction::Resolve(entry.path.clone(), true));
                    ui.close();
                }
                if ui.button("Mark as Resolved").clicked() {
                    deferred.push(RowAction::Stage(vec![entry.path.clone()]));
                    ui.close();
                }
            }
        }
        ui.separator();
        if ui.button("Copy Path").clicked() {
            ui.ctx().copy_text(entry.path.clone());
            ui.close();
        }
    });
}

/// Commit message length hints: the subject should fit in 50 characters
/// (72 at most) and body lines in 72.
fn message_guide(ui: &mut Ui, message: &str, colors: &UiColors) {
    let mut lines = message.lines();
    let Some(subject) = lines.next().filter(|s| !s.is_empty()) else {
        return;
    };
    let n = subject.chars().count();
    let color = match n {
        0..=50 => colors.text_muted.0,
        51..=72 => colors.warning.0,
        _ => colors.error.0,
    };
    let mut notes = vec![format!("Subject {n}/50")];
    let rest: Vec<&str> = lines.collect();
    if rest.first().is_some_and(|l| !l.trim().is_empty()) {
        notes.push("add a blank line after the subject".to_owned());
    }
    let long = rest.iter().filter(|l| l.chars().count() > 72).count();
    if long > 0 {
        notes.push(format!("{long} body line(s) over 72"));
    }
    let warn = n > 50 || notes.len() > 1;
    ui.label(
        RichText::new(notes.join(" · "))
            .small()
            .color(if warn && n <= 50 {
                colors.warning.0
            } else {
                color
            }),
    )
    .on_hover_text("Git convention: subject ≤ 50 characters, blank line, body wrapped at 72");
}

/// Why the "New Worktree" form can't be submitted yet, if anything.
fn new_worktree_problem(form: &NewWorktree, sc: &SourceControl) -> Option<String> {
    if form.new_branch {
        let name = form.branch.trim();
        if name.is_empty() {
            return Some("Name the new branch.".to_owned());
        }
        if name.contains(char::is_whitespace) || name.contains("..") || name.ends_with('/') {
            return Some("That isn't a valid branch name.".to_owned());
        }
        if sc.branches.iter().any(|b| !b.remote && b.name == name) {
            return Some(format!("A branch named “{name}” already exists."));
        }
    } else if form.existing.is_empty() {
        return Some("Choose a branch.".to_owned());
    }
    let path = form.path.trim();
    if path.is_empty() {
        return Some("Choose a folder.".to_owned());
    }
    let path = Path::new(path);
    if !path.is_absolute() {
        return Some("Use a full path for the folder.".to_owned());
    }
    if path.exists() && path.read_dir().map_or(true, |mut d| d.next().is_some()) {
        return Some("That folder already exists and isn't empty.".to_owned());
    }
    None
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Reveal in File Explorer"
    } else {
        "Open Containing Folder"
    }
}

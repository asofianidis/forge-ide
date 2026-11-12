//! The Source Control sidebar: branch and sync controls, the commit box,
//! staged / unstaged / conflicted files, stashes, branches and history.

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, Sense, Ui, UiBuilder, vec2};

use super::status::{Change, Entry};
use super::{Branch, RepoState, SourceControl};
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
}

/// Actions that need a confirmation dialog first.
#[derive(Debug, Clone)]
enum Confirm {
    Discard(Vec<Entry>),
    DeleteBranch(String),
    ForcePush,
    DropStash(String),
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
        let mut action = None;
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
        ui.add(
            egui::TextEdit::multiline(&mut self.message)
                .id(id)
                .hint_text(hint)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );

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
                ui.checkbox(&mut self.amend, "Amend previous commit");
            });
        });
        if self.amend {
            ui.label(
                RichText::new("Amending: leave the message empty to keep the previous one.")
                    .small()
                    .color(colors.warning.0),
            );
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
        section(
            ui,
            "History",
            sc.log.len(),
            false,
            colors,
            |_| {},
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
        };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("scm_confirm")).show(ui.ctx(), |ui| {
            ui.set_max_width(360.0);
            ui.heading(title);
            ui.add_space(4.0);
            ui.label(body);
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

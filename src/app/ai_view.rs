//! Connects the BYO AI view to the rest of the app: the sidebar, thread
//! tabs, and the one-click commit / push / pull request flow.

use std::path::PathBuf;

use super::ForgeApp;
use crate::ai::{FlowKind, FlowPlan, GitSummary, SidebarAction, ThreadAction};
use crate::git::{RepoState, SourceControl};
use crate::panes::{AiPane, Pane};

impl ForgeApp {
    /// Per-frame upkeep: agent output, the git flow, and keeping Source
    /// Control's "Generate" buttons on the selected text model.
    pub(super) fn poll_ai(&mut self) {
        self.ai.in_repo = self.source_control.state == RepoState::Ready;
        self.ai.current_branch = self.source_control.status.branch.head.clone();
        // Follow the worktree of the thread on screen, if it has one.
        match self.visible_worktree() {
            Some(path) => {
                if self.ai_worktree_sc.as_ref().is_none_or(|(p, _)| *p != path) {
                    let sc = SourceControl::new(self.egui_ctx.clone(), Some(&path));
                    self.ai_worktree_sc = Some((path, sc));
                }
                if let Some((_, sc)) = &mut self.ai_worktree_sc {
                    sc.poll();
                }
            }
            None => self.ai_worktree_sc = None,
        }
        if self.ai.poll() {
            // An agent finished a turn; it may have changed files.
            self.source_control.refresh();
            if let Some((_, sc)) = &mut self.ai_worktree_sc {
                sc.refresh();
            }
        }
        let text_gen = self.ai.text_generator(&self.settings.ai);
        if let Some((_, sc)) = &mut self.ai_worktree_sc {
            sc.text_gen = text_gen.clone();
            for event in sc.take_events() {
                self.status = match event {
                    crate::git::Event::Done(msg) => msg,
                    crate::git::Event::Failed(msg) => format!("Error: {msg}"),
                };
            }
        }
        self.source_control.text_gen = text_gen;
        for msg in self.ai.take_status() {
            self.status = msg;
        }
        if let Some(result) = self.ai.poll_flow() {
            self.status = match result {
                Ok(done) => done.summary,
                Err(e) => format!("Error: {e}"),
            };
            self.source_control.refresh();
            if let Some((_, sc)) = &mut self.ai_worktree_sc {
                sc.refresh();
            }
            if self.github.is_ready() {
                self.github.refresh();
            }
        }
        // Thread titles change as conversations start.
        for pane in &mut self.panes {
            if let Pane::Ai(p) = pane
                && let Some(t) = self.ai.thread(&p.id)
                && t.title != p.title
            {
                p.title = t.title.clone();
            }
        }
    }

    /// The worktree folder of the AI thread on screen, if it has one.
    fn visible_worktree(&self) -> Option<PathBuf> {
        let Some(crate::panes::Pane::Ai(p)) = self.active_pane.and_then(|i| self.panes.get(i))
        else {
            return None;
        };
        self.ai
            .thread(&p.id)?
            .worktree
            .as_ref()
            .map(|w| w.path.clone())
    }

    /// The repository the git button acts on: the visible thread's
    /// worktree, else the open folder.
    fn ai_git(&self) -> &SourceControl {
        match (&self.ai_worktree_sc, self.visible_worktree()) {
            (Some((p, sc)), Some(v)) if *p == v => sc,
            _ => &self.source_control,
        }
    }

    fn git_summary(&self) -> GitSummary {
        let sc = self.ai_git();
        let b = &sc.status.branch;
        let pr = b
            .head
            .as_deref()
            .and_then(|h| self.github.pr_for_branch(h))
            .map(|p| (p.number, p.title.clone()));
        GitSummary {
            is_repo: sc.state == RepoState::Ready,
            branch: b.head.clone(),
            changes: sc.status.entries.len(),
            staged: sc.status.staged().count(),
            conflicts: sc.status.conflicts().count(),
            ahead: b.ahead as usize,
            has_upstream: b.upstream.is_some(),
            has_remote: !sc.remotes.is_empty(),
            gh_ready: self.github.is_ready(),
            default_branch: self.github.repo.default_branch_ref.name.clone(),
            pr,
            busy: sc.busy().is_some(),
        }
    }

    pub(super) fn ai_sidebar(&mut self, ui: &mut egui::Ui) {
        let colors = self.current_theme().ui.clone();
        let mut ai_settings = self.settings.ai.clone();
        ui.add_space(4.0);
        let actions = self.ai.sidebar(ui, &mut ai_settings, &colors);
        self.handle_ai_actions(actions);
        if ai_settings != self.settings.ai {
            self.update_settings(|s| s.ai = ai_settings);
        }
    }

    /// The commit / push / PR button at the right end of the tab bar.
    /// `ui` lays out right to left.
    pub(super) fn ai_git_button(&mut self, ui: &mut egui::Ui) {
        // Only alongside an AI thread tab that is being viewed.
        let ai_tab = matches!(
            self.active_pane.and_then(|i| self.panes.get(i)),
            Some(Pane::Ai(_))
        );
        if self.workspace.is_none() || !ai_tab {
            return;
        }
        let colors = self.current_theme().ui.clone();
        let git = self.git_summary();
        let actions = self.ai.git_button(ui, &self.settings.ai, &git, &colors);
        self.handle_ai_actions(actions);
    }

    fn handle_ai_actions(&mut self, actions: Vec<SidebarAction>) {
        for action in actions {
            match action {
                SidebarAction::OpenThread(id) => self.open_ai_thread(id),
                SidebarAction::ThreadDeleted(id) => {
                    if let Some(i) = self
                        .panes
                        .iter()
                        .position(|p| matches!(p, Pane::Ai(a) if a.id == id))
                    {
                        self.close_pane(i);
                    }
                }
                SidebarAction::StartFlow(kind) => self.start_ai_flow(kind),
                SidebarAction::OpenPr(n) => self.open_pull_request(n),
                SidebarAction::OpenSettings => self.settings_open = true,
                SidebarAction::OpenFolderInNewWindow(path) => self.open_folder_in_new_window(&path),
            }
        }
    }

    pub(super) fn open_ai_thread(&mut self, id: String) {
        let title = self
            .ai
            .thread(&id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        self.open_pane(Pane::Ai(AiPane { id, title }));
    }

    pub(super) fn new_ai_thread(&mut self) {
        match self.ai.new_thread(&self.settings.ai) {
            Some(id) => self.open_ai_thread(id),
            None => self.status = "Open a folder to start an AI thread".to_owned(),
        }
    }

    /// Draws the thread tab at `idx`.
    pub(super) fn ai_thread_pane(&mut self, ui: &mut egui::Ui, id: &str) {
        let colors = self.current_theme().ui.clone();
        let mut ai_settings = self.settings.ai.clone();
        let action = self.ai.thread_view(ui, id, &mut ai_settings, &colors);
        if matches!(action, Some(ThreadAction::SettingsChanged)) || ai_settings != self.settings.ai
        {
            self.update_settings(|s| s.ai = ai_settings);
        }
    }

    fn start_ai_flow(&mut self, kind: FlowKind) {
        if self.ai.flow.is_some() {
            return;
        }
        let sc = self.ai_git();
        let Some(git) = sc.git_cli() else {
            self.status = "This folder isn't a git repository".to_owned();
            return;
        };
        let b = &sc.status.branch;
        let branch = b.head.clone().unwrap_or_default();
        let base = self.github.repo.default_branch_ref.name.clone();
        let publish_to = if b.upstream.is_none() {
            match sc.default_remote() {
                Some(r) => Some(r),
                None if kind.pushes() => {
                    self.status = "This repository has no remote to push to".to_owned();
                    return;
                }
                None => None,
            }
        } else {
            None
        };
        let commit_message = if kind.commits() {
            sc.commit_message_job()
                .map(|j| Box::new(j) as crate::ai::CommitJob)
        } else {
            None
        };
        let pr_text = if kind.opens_pr() {
            sc.pr_text_job(&base)
                .map(|j| Box::new(j) as crate::ai::PrJob)
        } else {
            None
        };
        let plan = FlowPlan {
            kind,
            git,
            gh: self.github.cli(),
            commit_message,
            stage_all: sc.status.staged().next().is_none(),
            publish_to,
            branch,
            base,
            pr_text,
        };
        self.status = format!("{}…", kind.label());
        self.ai.start_flow(plan);
    }
}

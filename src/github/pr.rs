//! The pull request view, shown as a tab in the editor area: conversation,
//! commits and changed files, with reviewing, commenting (including inline
//! comments on diff lines), merging and checkout.

use std::collections::HashMap;

use egui::{Color32, RichText, Ui};

use super::GhEvent;
use super::list::{check_glyph, label_chip, review_badge, state_color, state_label};
use super::model::{self, PrDetail, ReviewComment};
use crate::cli::Cli;
use crate::git::diff::{self, DiffLine, FileDiff, LineKind};
use crate::task::{self, Pending};
use crate::theme::UiColors;
use crate::ui::diff_view::{self, DiffColors, DiffHooks, LineRef, Side, line_anchor};
use crate::ui::fmt;
use crate::ui::icons::{self, Icon};
use crate::ui::markdown;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Conversation,
    Commits,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewKind {
    Comment,
    Approve,
    RequestChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    fn label(self) -> &'static str {
        match self {
            Self::Squash => "Squash and merge",
            Self::Merge => "Create a merge commit",
            Self::Rebase => "Rebase and merge",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Self::Squash => "--squash",
            Self::Merge => "--merge",
            Self::Rebase => "--rebase",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InlineDraft {
    path: String,
    side: Side,
    line: u32,
    body: String,
    /// The line's current text, for "Suggest change".
    line_text: String,
}

enum Submit {
    Inline(InlineDraft),
    Reply(u64, String),
    /// Add to the pending review instead of posting now.
    Pending(InlineDraft),
    RemovePending(usize),
    /// Resolve (true) or unresolve a thread by GraphQL id.
    Resolve(String, bool),
    ToggleViewed(String, bool),
    ApplySuggestion {
        path: String,
        start: u32,
        end: u32,
        text: String,
    },
}

/// Label of the review action, so a successful submit clears pending comments.
const SUBMIT_REVIEW: &str = "Submit review";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Merge,
    Close,
}

/// Inline comments grouped into threads and indexed by diff position.
#[derive(Default)]
struct Threads {
    /// (path, side, line) → indices of thread roots.
    by_line: HashMap<(String, Side, u32), Vec<usize>>,
    /// root comment id → indices of replies, oldest first.
    replies: HashMap<u64, Vec<usize>>,
    /// path → roots of comments whose line no longer exists in the diff.
    outdated: HashMap<String, Vec<usize>>,
}

impl Threads {
    fn build(comments: &[ReviewComment]) -> Self {
        let mut t = Threads::default();
        for (i, c) in comments.iter().enumerate() {
            if let Some(root) = c.in_reply_to_id {
                t.replies.entry(root).or_default().push(i);
                continue;
            }
            let side = if c.side == "LEFT" {
                Side::Left
            } else {
                Side::Right
            };
            match c.line {
                Some(line) => t
                    .by_line
                    .entry((c.path.clone(), side, line))
                    .or_default()
                    .push(i),
                None => t.outdated.entry(c.path.clone()).or_default().push(i),
            }
        }
        t
    }
}

type Action = (String, bool, Pending<Result<String, String>>);
type DiffResult = Result<Vec<FileDiff>, String>;

pub struct PrPane {
    ctx: egui::Context,
    cli: Cli,
    pub number: u64,
    /// The signed-in user.
    me: String,
    detail: Option<PrDetail>,
    detail_task: Option<Pending<Result<PrDetail, String>>>,
    error: Option<String>,
    diff: Option<Vec<FileDiff>>,
    diff_task: Option<Pending<Result<Vec<FileDiff>, String>>>,
    diff_error: Option<String>,
    comments: Vec<ReviewComment>,
    threads: Threads,
    comments_task: Option<Pending<Result<Vec<ReviewComment>, String>>>,
    /// Commit expanded in the Commits tab and its diff.
    commit: Option<(String, Option<DiffResult>)>,
    commit_task: Option<Pending<Result<Vec<FileDiff>, String>>>,

    tab: Tab,
    comment_draft: String,
    review_body: String,
    review_kind: ReviewKind,
    inline: Option<InlineDraft>,
    reply: Option<(u64, String)>,
    reviewer_input: String,
    /// People who can be requested as reviewers, loaded on first use.
    people: Option<Vec<model::Person>>,
    people_task: Option<Pending<Vec<model::Person>>>,
    reviewer_picker_open: bool,
    merge_method: MergeMethod,
    delete_branch: bool,
    confirm: Option<Confirm>,
    scroll_to_file: Option<usize>,
    /// Running action: label, whether it changes local git state, result.
    action: Option<Action>,
    events: Vec<GhEvent>,

    extra: Option<model::PrExtra>,
    extra_task: Option<Pending<Result<model::PrExtra, String>>>,
    /// Edits that don't block other actions (viewed files, labels, …).
    quiet: Vec<(String, Pending<Result<String, String>>)>,
    /// Review comments collected before submitting a review.
    pending: Vec<InlineDraft>,
    /// Resolved threads the user expanded.
    expanded_resolved: std::collections::HashSet<u64>,
    editing_title: Option<String>,
    editing_body: Option<String>,
    labels: Option<Vec<model::RepoLabel>>,
    labels_task: Option<Pending<Vec<model::RepoLabel>>>,
    milestones: Option<Vec<model::Milestone>>,
    milestones_task: Option<Pending<Vec<model::Milestone>>>,
    assignee_input: String,
    /// Repository root and checked-out branch, for applying suggestions.
    local_root: Option<std::path::PathBuf>,
    local_branch: Option<String>,
}

impl PrPane {
    pub fn new(ctx: egui::Context, cli: Cli, number: u64, me: String) -> Self {
        let mut pane = Self {
            ctx,
            cli,
            number,
            me,
            detail: None,
            detail_task: None,
            error: None,
            diff: None,
            diff_task: None,
            diff_error: None,
            comments: Vec::new(),
            threads: Threads::default(),
            comments_task: None,
            commit: None,
            commit_task: None,
            tab: Tab::Conversation,
            comment_draft: String::new(),
            review_body: String::new(),
            review_kind: ReviewKind::Comment,
            inline: None,
            reply: None,
            reviewer_input: String::new(),
            people: None,
            people_task: None,
            reviewer_picker_open: false,
            merge_method: MergeMethod::Squash,
            delete_branch: false,
            confirm: None,
            scroll_to_file: None,
            action: None,
            events: Vec::new(),
            extra: None,
            extra_task: None,
            quiet: Vec::new(),
            pending: Vec::new(),
            expanded_resolved: Default::default(),
            editing_title: None,
            editing_body: None,
            labels: None,
            labels_task: None,
            milestones: None,
            milestones_task: None,
            assignee_input: String::new(),
            local_root: None,
            local_branch: None,
        };
        pane.reload();
        pane
    }

    /// Tab title.
    pub fn title(&self) -> String {
        match &self.detail {
            Some(d) => format!("#{} {}", d.number, d.title),
            None => format!("#{}", self.number),
        }
    }

    pub fn take_events(&mut self) -> Vec<GhEvent> {
        std::mem::take(&mut self.events)
    }

    /// Reloads details and comments, and the diff if it was loaded.
    pub fn reload(&mut self) {
        let cli = self.cli.clone();
        let n = self.number.to_string();
        self.detail_task = Some(Pending::spawn(&self.ctx, move || {
            let out = cli.run(&["pr", "view", &n, "--json", model::DETAIL_FIELDS])?;
            serde_json::from_str(&out).map_err(|e| format!("unexpected response from gh: {e}"))
        }));
        self.load_comments();
        self.load_extra();
        if self.diff.is_some() || self.tab == Tab::Files {
            self.load_diff();
        }
    }

    /// Thread ids/resolution and viewed files (GraphQL only).
    fn load_extra(&mut self) {
        let cli = self.cli.clone();
        let n = format!("n={}", self.number);
        self.extra_task = Some(Pending::spawn(&self.ctx, move || {
            let query = format!("query={}", model::EXTRA_QUERY);
            let out = cli.run(&[
                "api",
                "graphql",
                "-f",
                &query,
                "-F",
                "owner={owner}",
                "-F",
                "repo={repo}",
                "-F",
                &n,
            ])?;
            model::parse_pr_extra(&out)
        }));
    }

    /// Where this PR could be applied locally (for suggestions).
    pub fn set_local(&mut self, root: Option<&std::path::Path>, branch: Option<String>) {
        self.local_root = root.map(std::path::Path::to_path_buf);
        self.local_branch = branch;
    }

    /// Whether the PR's branch is checked out here, so suggestions can be
    /// applied to the working tree.
    fn can_apply_suggestions(&self) -> bool {
        self.local_root.is_some()
            && self
                .detail
                .as_ref()
                .is_some_and(|d| Some(&d.head_ref_name) == self.local_branch.as_ref())
    }

    /// Runs a command without blocking other actions; refreshes when done.
    fn quiet(
        &mut self,
        label: &str,
        work: impl FnOnce(&Cli) -> Result<String, String> + Send + 'static,
    ) {
        let cli = self.cli.clone();
        self.quiet.push((
            label.to_owned(),
            Pending::spawn(&self.ctx, move || work(&cli)),
        ));
    }

    fn quiet_args(&mut self, label: &str, args: Vec<String>) {
        self.quiet(label, move |cli| cli.run(&args));
    }

    fn graphql(&mut self, label: &str, mutation: &'static str, vars: Vec<(&'static str, String)>) {
        self.quiet(label, move |cli| {
            let mut args = vec![
                "api".to_owned(),
                "graphql".to_owned(),
                "-f".to_owned(),
                format!("query={mutation}"),
            ];
            for (k, v) in vars {
                args.extend(["-f".to_owned(), format!("{k}={v}")]);
            }
            cli.run(&args)
        });
    }

    fn resolve_thread(&mut self, thread: String, resolve: bool) {
        if let Some(extra) = &mut self.extra {
            for (tid, resolved) in extra.threads.values_mut() {
                if *tid == thread {
                    *resolved = resolve;
                }
            }
        }
        let (label, m) = if resolve {
            (
                "Resolve conversation",
                "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}",
            )
        } else {
            (
                "Unresolve conversation",
                "mutation($id:ID!){unresolveReviewThread(input:{threadId:$id}){thread{isResolved}}}",
            )
        };
        self.graphql(label, m, vec![("id", thread)]);
    }

    fn set_viewed(&mut self, path: String, viewed: bool) {
        let Some(extra) = &mut self.extra else { return };
        let pr = extra.pr_id.clone();
        if viewed {
            extra.viewed.insert(path.clone());
        } else {
            extra.viewed.remove(&path);
        }
        let m = if viewed {
            "mutation($pr:ID!,$path:String!){markFileAsViewed(input:{pullRequestId:$pr,path:$path}){clientMutationId}}"
        } else {
            "mutation($pr:ID!,$path:String!){unmarkFileAsViewed(input:{pullRequestId:$pr,path:$path}){clientMutationId}}"
        };
        self.graphql("", m, vec![("pr", pr), ("path", path)]);
    }

    /// Replaces lines `start..=end` of the local file with a suggestion.
    fn apply_suggestion(&mut self, path: &str, start: u32, end: u32, text: &str) {
        let Some(root) = &self.local_root else { return };
        let file = root.join(path);
        let result = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|content| replace_lines(&content, start, end, text))
            .and_then(|new| std::fs::write(&file, new).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                self.events.push(GhEvent::Done(format!(
                    "Applied suggestion to {path}; commit and push to update the PR"
                )));
                self.events.push(GhEvent::GitChanged);
            }
            Err(e) => self
                .events
                .push(GhEvent::Failed(format!("Could not apply suggestion: {e}"))),
        }
    }

    fn ensure_labels(&mut self) {
        if self.labels.is_none() && self.labels_task.is_none() {
            let cli = self.cli.clone();
            self.labels_task = Some(Pending::spawn(&self.ctx, move || {
                cli.run(&[
                    "label",
                    "list",
                    "--limit",
                    "200",
                    "--json",
                    "name,color,description",
                ])
                .ok()
                .and_then(|o| serde_json::from_str(&o).ok())
                .unwrap_or_default()
            }));
        }
    }

    fn ensure_milestones(&mut self) {
        if self.milestones.is_none() && self.milestones_task.is_none() {
            let cli = self.cli.clone();
            self.milestones_task = Some(Pending::spawn(&self.ctx, move || {
                cli.run(&[
                    "api",
                    "repos/{owner}/{repo}/milestones?state=open&per_page=100",
                ])
                .ok()
                .and_then(|o| serde_json::from_str(&o).ok())
                .unwrap_or_default()
            }));
        }
    }

    /// Loads assignable users and contributors for the reviewer picker.
    /// Either source may fail (e.g. no permission); the other still helps.
    fn load_people(&mut self) {
        let cli = self.cli.clone();
        self.people_task = Some(Pending::spawn(&self.ctx, move || {
            let query = format!("query={}", model::ASSIGNABLE_QUERY);
            let assignable = cli
                .run(&[
                    "api",
                    "graphql",
                    "-f",
                    &query,
                    "-F",
                    "owner={owner}",
                    "-F",
                    "repo={repo}",
                ])
                .map(|out| model::parse_assignable(&out))
                .unwrap_or_default();
            let contributors = cli
                .run(&["api", "repos/{owner}/{repo}/contributors?per_page=100"])
                .ok()
                .and_then(|out| serde_json::from_str(&out).ok())
                .unwrap_or_default();
            model::merge_people(assignable, contributors)
        }));
    }

    fn load_comments(&mut self) {
        let cli = self.cli.clone();
        let path = format!(
            "repos/{{owner}}/{{repo}}/pulls/{}/comments?per_page=100",
            self.number
        );
        self.comments_task = Some(Pending::spawn(&self.ctx, move || {
            let out = cli.run(&["api", "--paginate", &path])?;
            model::parse_paginated(&out)
        }));
    }

    fn load_diff(&mut self) {
        let cli = self.cli.clone();
        let n = self.number.to_string();
        self.diff_task = Some(Pending::spawn(&self.ctx, move || {
            let out = cli.run(&["pr", "diff", &n, "--color", "never"])?;
            Ok(diff::parse(&out))
        }));
    }

    fn load_commit(&mut self, oid: String) {
        let cli = self.cli.clone();
        let path = format!("repos/{{owner}}/{{repo}}/commits/{oid}");
        self.commit = Some((oid, None));
        self.commit_task = Some(Pending::spawn(&self.ctx, move || {
            let out = cli.run(&["api", "-H", "Accept: application/vnd.github.diff", &path])?;
            Ok(diff::parse(&out))
        }));
    }

    /// Runs a gh command in the background; reloads when it finishes.
    fn run(
        &mut self,
        label: &str,
        git_changed: bool,
        work: impl FnOnce(&Cli) -> Result<String, String> + Send + 'static,
    ) {
        if self.action.is_some() {
            return;
        }
        let cli = self.cli.clone();
        self.action = Some((
            label.to_owned(),
            git_changed,
            Pending::spawn(&self.ctx, move || work(&cli)),
        ));
    }

    fn poll(&mut self) {
        if let Some(result) = task::poll(&mut self.detail_task) {
            match result {
                Ok(d) => {
                    self.detail = Some(d);
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(result) = task::poll(&mut self.diff_task) {
            match result {
                Ok(d) => {
                    self.diff = Some(d);
                    self.diff_error = None;
                }
                Err(e) => self.diff_error = Some(e),
            }
        }
        if let Some(people) = task::poll(&mut self.people_task) {
            self.people = Some(people);
        }
        if let Some(Ok(extra)) = task::poll(&mut self.extra_task) {
            self.extra = Some(extra);
        }
        if let Some(labels) = task::poll(&mut self.labels_task) {
            self.labels = Some(labels);
        }
        if let Some(m) = task::poll(&mut self.milestones_task) {
            self.milestones = Some(m);
        }
        let mut done = Vec::new();
        self.quiet.retain(|(label, t)| match t.try_take() {
            Some(r) => {
                done.push((label.clone(), r));
                false
            }
            None => true,
        });
        if !done.is_empty() {
            for (label, r) in done {
                match r {
                    Ok(_) if label.is_empty() => {}
                    Ok(_) => self.events.push(GhEvent::Done(format!("{label}: done"))),
                    Err(e) => self.events.push(GhEvent::Failed(format!(
                        "{} failed: {e}",
                        if label.is_empty() { "Update" } else { &label }
                    ))),
                }
            }
            if self.quiet.is_empty() {
                // Re-read what changed without reloading the diff.
                let cli = self.cli.clone();
                let n = self.number.to_string();
                self.detail_task = Some(Pending::spawn(&self.ctx, move || {
                    let out = cli.run(&["pr", "view", &n, "--json", model::DETAIL_FIELDS])?;
                    serde_json::from_str(&out)
                        .map_err(|e| format!("unexpected response from gh: {e}"))
                }));
                self.load_extra();
                self.events.push(GhEvent::PrsChanged);
            }
        }
        if let Some(Ok(comments)) = task::poll(&mut self.comments_task) {
            self.threads = Threads::build(&comments);
            self.comments = comments;
        }
        if let Some(result) = task::poll(&mut self.commit_task)
            && let Some((_, slot)) = &mut self.commit
        {
            *slot = Some(result);
        }
        if let Some((label, git_changed, pending)) = &self.action
            && let Some(result) = pending.try_take()
        {
            match result {
                Ok(_) => {
                    if label == SUBMIT_REVIEW {
                        self.pending.clear();
                    }
                    self.events.push(GhEvent::Done(format!("{label}: done")));
                }
                Err(e) => self
                    .events
                    .push(GhEvent::Failed(format!("{label} failed: {e}"))),
            }
            if *git_changed {
                self.events.push(GhEvent::GitChanged);
            }
            self.events.push(GhEvent::PrsChanged);
            self.action = None;
            self.reload();
        }
    }

    // ---- actions ---------------------------------------------------------

    fn submit_review(&mut self) {
        if !self.pending.is_empty() {
            self.submit_review_with_comments();
            return;
        }
        let n = self.number.to_string();
        let body = std::mem::take(&mut self.review_body);
        let (flag, label) = match self.review_kind {
            ReviewKind::Comment => ("--comment", "Review"),
            ReviewKind::Approve => ("--approve", "Approve"),
            ReviewKind::RequestChanges => ("--request-changes", "Request changes"),
        };
        self.run(label, false, move |cli| {
            if body.trim().is_empty() {
                cli.run(&["pr", "review", &n, flag])
            } else {
                cli.run_with_input(&["pr", "review", &n, flag, "--body-file", "-"], &body)
            }
        });
    }

    /// Submits the pending inline comments together with the review.
    fn submit_review_with_comments(&mut self) {
        let Some(oid) = self.detail.as_ref().map(|d| d.head_ref_oid.clone()) else {
            return;
        };
        let event = match self.review_kind {
            ReviewKind::Comment => "COMMENT",
            ReviewKind::Approve => "APPROVE",
            ReviewKind::RequestChanges => "REQUEST_CHANGES",
        };
        let comments: Vec<serde_json::Value> = self
            .pending
            .iter()
            .map(|p| {
                serde_json::json!({
                    "path": p.path,
                    "line": p.line,
                    "side": p.side.api_name(),
                    "body": p.body,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "commit_id": oid,
            "body": std::mem::take(&mut self.review_body),
            "event": event,
            "comments": comments,
        })
        .to_string();
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{}/reviews", self.number);
        self.run(SUBMIT_REVIEW, false, move |cli| {
            cli.run_with_input(
                &["api", "--method", "POST", &path, "--input", "-"],
                &payload,
            )
        });
    }

    fn submit_comment(&mut self) {
        let n = self.number.to_string();
        let body = std::mem::take(&mut self.comment_draft);
        self.run("Comment", false, move |cli| {
            cli.run_with_input(&["pr", "comment", &n, "--body-file", "-"], &body)
        });
    }

    fn submit_inline(&mut self, draft: InlineDraft) {
        let Some(oid) = self.detail.as_ref().map(|d| d.head_ref_oid.clone()) else {
            return;
        };
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{}/comments", self.number);
        let payload = serde_json::json!({
            "body": draft.body,
            "commit_id": oid,
            "path": draft.path,
            "line": draft.line,
            "side": draft.side.api_name(),
        })
        .to_string();
        self.run("Comment on line", false, move |cli| {
            cli.run_with_input(
                &["api", "--method", "POST", &path, "--input", "-"],
                &payload,
            )
        });
    }

    fn submit_reply(&mut self, root: u64, body: String) {
        let path = format!(
            "repos/{{owner}}/{{repo}}/pulls/{}/comments/{root}/replies",
            self.number
        );
        let payload = serde_json::json!({ "body": body }).to_string();
        self.run("Reply", false, move |cli| {
            cli.run_with_input(
                &["api", "--method", "POST", &path, "--input", "-"],
                &payload,
            )
        });
    }

    fn simple(&mut self, label: &str, git_changed: bool, args: Vec<String>) {
        self.run(label, git_changed, move |cli| cli.run(&args));
    }

    fn args(&self, parts: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = vec!["pr".into(), parts[0].into(), self.number.to_string()];
        v.extend(parts[1..].iter().map(|s| (*s).to_owned()));
        v
    }

    // ---- UI --------------------------------------------------------------

    pub fn show(&mut self, ui: &mut Ui, colors: &UiColors) {
        self.poll();
        let Some(detail) = self.detail.clone() else {
            ui.add_space(20.0);
            match &self.error {
                Some(e) => {
                    ui.colored_label(
                        colors.error.0,
                        format!("Could not load pull request #{}: {e}", self.number),
                    );
                    if ui.button("Retry").clicked() {
                        self.reload();
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Loading pull request #{}…", self.number));
                    });
                }
            }
            return;
        };

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                self.header(ui, &detail, colors);
            });
        ui.separator();
        match self.tab {
            Tab::Conversation => {
                egui::ScrollArea::vertical()
                    .id_salt(("pr_conv", self.number))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| self.conversation(ui, &detail, colors));
                    });
            }
            Tab::Commits => {
                egui::ScrollArea::vertical()
                    .id_salt(("pr_commits", self.number))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| self.commits(ui, &detail, colors));
                    });
            }
            Tab::Files => self.files(ui, colors),
        }
        self.confirm_dialog(ui, &detail);
    }

    fn header(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        let mut save_title = None;
        ui.horizontal(|ui| {
            if let Some(title) = &mut self.editing_title {
                ui.add(
                    egui::TextEdit::singleline(title)
                        .font(egui::FontId::proportional(17.0))
                        .desired_width(ui.available_width() - 140.0),
                );
                if ui.button("Save").clicked() && !title.trim().is_empty() {
                    save_title = Some(title.trim().to_owned());
                }
                if ui.button("Cancel").clicked() {
                    self.editing_title = None;
                }
                return;
            }
            ui.label(RichText::new(&d.title).size(19.0).strong());
            ui.label(
                RichText::new(format!("#{}", d.number))
                    .size(19.0)
                    .color(colors.text_muted.0),
            );
            if ui
                .small_button("Edit")
                .on_hover_text("Edit the title")
                .clicked()
            {
                self.editing_title = Some(d.title.clone());
            }
        });
        if let Some(title) = save_title {
            self.editing_title = None;
            let args = self.args(&["edit", "--title", &title]);
            self.quiet_args("Edit title", args);
        }
        ui.horizontal_wrapped(|ui| {
            let color = state_color(&d.state, d.is_draft, colors);
            egui::Frame::new()
                .fill(color)
                .corner_radius(10)
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        icons::label(ui, Icon::PullRequest, Color32::WHITE, 14.0);
                        ui.label(
                            RichText::new(state_label(&d.state, d.is_draft))
                                .color(Color32::WHITE)
                                .strong(),
                        );
                    });
                });
            ui.label(RichText::new(d.author.display()).strong());
            let verb = if d.state == "MERGED" {
                "merged"
            } else {
                "wants to merge"
            };
            ui.label(format!("{verb} {} commit(s) into", d.commits.len()));
            ui.label(
                RichText::new(&d.base_ref_name)
                    .monospace()
                    .color(colors.link.0),
            );
            ui.label("from");
            ui.label(
                RichText::new(&d.head_ref_name)
                    .monospace()
                    .color(colors.link.0),
            )
            .on_hover_text("Click to copy")
            .clicked()
            .then(|| ui.ctx().copy_text(d.head_ref_name.clone()));
            ui.label(
                RichText::new(format!("· opened {}", fmt::relative_iso(&d.created_at)))
                    .color(colors.text_muted.0),
            );
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let threads = self
                .comments
                .iter()
                .filter(|c| c.in_reply_to_id.is_none())
                .count();
            let tabs = [
                (
                    Tab::Conversation,
                    format!("Conversation  {}", d.comments.len() + d.reviews.len()),
                ),
                (Tab::Commits, format!("Commits  {}", d.commits.len())),
                (Tab::Files, format!("Files changed  {}", d.changed_files)),
            ];
            for (tab, label) in tabs {
                let selected = self.tab == tab;
                let text = if selected {
                    RichText::new(label).strong()
                } else {
                    RichText::new(label)
                };
                let r = ui.selectable_label(selected, text);
                if selected {
                    ui.painter().hline(
                        r.rect.x_range().shrink(4.0),
                        r.rect.bottom() + 2.0,
                        egui::Stroke::new(2.0, colors.accent.0),
                    );
                }
                if r.clicked() {
                    self.tab = tab;
                    if tab == Tab::Files && self.diff.is_none() && self.diff_task.is_none() {
                        self.load_diff();
                    }
                }
            }
            if threads > 0 {
                ui.label(
                    RichText::new(format!("{threads} inline thread(s)"))
                        .small()
                        .color(colors.text_muted.0),
                );
            }
            ui.label(RichText::new(format!("+{}", d.additions)).color(colors.added.0));
            ui.label(RichText::new(format!("−{}", d.deletions)).color(colors.deleted.0));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Refresh, "Refresh").clicked() {
                    self.reload();
                }
                if icons::button(ui, Icon::External, "Open on GitHub").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&d.url));
                }
                if d.state == "OPEN"
                    && ui
                        .button("Checkout")
                        .on_hover_text(format!(
                            "Check out {} locally (gh pr checkout)",
                            d.head_ref_name
                        ))
                        .clicked()
                {
                    let args = self.args(&["checkout"]);
                    self.simple("Checkout", true, args);
                }
                if d.state == "OPEN" {
                    self.review_button(ui, d, colors);
                }
                if let Some((label, _, _)) = &self.action {
                    ui.spinner();
                    ui.label(RichText::new(format!("{label}…")).color(colors.text_muted.0));
                }
            });
        });
    }

    fn review_button(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        let label = match self.pending.len() {
            0 => "Review changes ▾".to_owned(),
            n => format!("Finish review ({n}) ▾"),
        };
        let button = ui.add(
            egui::Button::new(RichText::new(label).color(colors.accent_text.0))
                .fill(colors.accent.0),
        );
        egui::Popup::menu(&button)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(380.0);
                ui.label(RichText::new("Finish your review").strong());
                ui.add(
                    egui::TextEdit::multiline(&mut self.review_body)
                        .hint_text("Leave a comment (Markdown)")
                        .desired_rows(5)
                        .desired_width(f32::INFINITY),
                );
                let own = !self.me.is_empty() && self.me == d.author.login;
                ui.radio_value(&mut self.review_kind, ReviewKind::Comment, "Comment")
                    .on_hover_text("General feedback without explicit approval");
                ui.add_enabled_ui(!own, |ui| {
                    ui.radio_value(&mut self.review_kind, ReviewKind::Approve, "Approve")
                        .on_hover_text("Approve merging these changes")
                        .on_disabled_hover_text("You can't approve your own pull request");
                    ui.radio_value(
                        &mut self.review_kind,
                        ReviewKind::RequestChanges,
                        "Request changes",
                    )
                    .on_hover_text("Feedback that must be addressed before merging")
                    .on_disabled_hover_text("You can't request changes on your own pull request");
                });
                if own && self.review_kind != ReviewKind::Comment {
                    self.review_kind = ReviewKind::Comment;
                }
                let pending = self.pending.len();
                if pending > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{pending} pending comment(s) will be submitted with this review"
                        ))
                        .small()
                        .color(colors.warning.0),
                    );
                }
                // A review with inline comments may have an empty summary.
                let needs_body = self.review_kind != ReviewKind::Approve && pending == 0;
                let ok = !needs_body || !self.review_body.trim().is_empty();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            ok && self.action.is_none(),
                            egui::Button::new("Submit review"),
                        )
                        .on_disabled_hover_text("Write a comment first")
                        .clicked()
                    {
                        self.submit_review();
                        ui.close();
                    }
                });
            });
    }

    fn conversation(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        let wide = ui.available_width() > 820.0;
        if wide {
            let total = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(total - 270.0);
                    self.timeline(ui, d, colors);
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.set_width(240.0);
                    self.meta(ui, d, colors);
                });
            });
        } else {
            self.meta(ui, d, colors);
            ui.separator();
            self.timeline(ui, d, colors);
        }
    }

    fn meta(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        let heading = |ui: &mut Ui, t: &str| {
            ui.label(RichText::new(t).small().strong().color(colors.text_muted.0));
        };
        heading(ui, "Reviewers");
        // Latest review state per reviewer.
        let mut latest: Vec<(&str, &str)> = Vec::new();
        for r in &d.reviews {
            if r.state == "COMMENTED" && latest.iter().any(|(a, _)| *a == r.author.login) {
                continue;
            }
            latest.retain(|(a, _)| *a != r.author.login);
            latest.push((r.author.display(), r.state.as_str()));
        }
        if latest.is_empty() && d.review_requests.is_empty() {
            ui.label(RichText::new("No reviews").weak().small());
        }
        for (who, state) in &latest {
            let (text, color) = match *state {
                "APPROVED" => ("✔ approved", colors.added.0),
                "CHANGES_REQUESTED" => ("✖ changes requested", colors.deleted.0),
                "DISMISSED" => ("dismissed", colors.text_muted.0),
                _ => ("commented", colors.text_muted.0),
            };
            ui.horizontal(|ui| {
                ui.label(*who);
                ui.label(RichText::new(text).small().color(color));
            });
        }
        for r in &d.review_requests {
            ui.horizontal(|ui| {
                ui.label(r.display());
                ui.label(RichText::new("● pending").small().color(colors.warning.0));
            });
        }
        if d.state == "OPEN" {
            self.reviewer_picker(ui, d, colors);
        }
        if let Some((text, color)) = review_badge(&d.review_decision, colors) {
            ui.label(RichText::new(text).small().color(color));
        }
        ui.add_space(8.0);
        heading(ui, "Assignees");
        if d.assignees.is_empty() {
            ui.label(RichText::new("No one").weak().small());
        }
        let mut remove_assignee = None;
        for a in &d.assignees {
            ui.horizontal(|ui| {
                ui.label(a.display());
                if ui.small_button("×").on_hover_text("Unassign").clicked() {
                    remove_assignee = Some(a.login.clone());
                }
            });
        }
        let mut add_assignee = None;
        if !self.me.is_empty()
            && !d.assignees.iter().any(|a| a.login == self.me)
            && ui.link(RichText::new("Assign yourself").small()).clicked()
        {
            add_assignee = Some(self.me.clone());
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.assignee_input)
                .hint_text("Assign: search people")
                .desired_width(f32::INFINITY),
        );
        if !self.assignee_input.trim().is_empty() {
            if self.people.is_none() && self.people_task.is_none() {
                self.load_people();
            }
            let exclude: Vec<&str> = d.assignees.iter().map(|a| a.login.as_str()).collect();
            if let Some(people) = &self.people {
                for p in model::search_people(people, &self.assignee_input, &exclude)
                    .into_iter()
                    .take(5)
                {
                    if ui
                        .add(
                            egui::Button::new(&p.login)
                                .right_text(RichText::new(p.detail()).small().weak())
                                .frame(false),
                        )
                        .clicked()
                    {
                        add_assignee = Some(p.login.clone());
                    }
                }
            }
        }
        if let Some(login) = add_assignee {
            self.assignee_input.clear();
            let args = self.args(&["edit", "--add-assignee", &login]);
            self.quiet_args(&format!("Assign {login}"), args);
        }
        if let Some(login) = remove_assignee {
            let args = self.args(&["edit", "--remove-assignee", &login]);
            self.quiet_args(&format!("Unassign {login}"), args);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            heading(ui, "Labels");
            let edit = ui.small_button("Edit");
            if edit.clicked() {
                self.ensure_labels();
            }
            egui::Popup::menu(&edit)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    ui.set_min_width(320.0);
                    let Some(all) = self.labels.clone() else {
                        ui.spinner();
                        return;
                    };
                    if all.is_empty() {
                        ui.label(RichText::new("This repository has no labels").weak());
                    }
                    let height = (all.len() as f32 * 26.0).min(300.0);
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .min_scrolled_height(height)
                        .show(ui, |ui| {
                            for l in &all {
                                let on = d.labels.iter().any(|x| x.name == l.name);
                                let row = ui
                                    .horizontal(|ui| {
                                        ui.label(
                                            RichText::new(if on { "✔" } else { " " })
                                                .monospace()
                                                .color(colors.accent.0),
                                        );
                                        label_chip(ui, &l.name, &l.color);
                                        if !l.description.is_empty() {
                                            ui.label(RichText::new(&l.description).small().weak());
                                        }
                                    })
                                    .response
                                    .interact(egui::Sense::click())
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if row.clicked() {
                                    let flag = if on { "--remove-label" } else { "--add-label" };
                                    let args = self.args(&["edit", flag, &l.name]);
                                    self.quiet_args(&format!("Label {}", l.name), args);
                                }
                            }
                        });
                });
        });
        if d.labels.is_empty() {
            ui.label(RichText::new("None yet").weak().small());
        }
        ui.horizontal_wrapped(|ui| {
            for l in &d.labels {
                label_chip(ui, &l.name, &l.color);
            }
        });

        ui.add_space(8.0);
        heading(ui, "Milestone");
        self.ensure_milestones();
        let current = d.milestone.as_ref().map(|m| m.title.clone());
        let mut pick: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt(("milestone", d.number))
            .selected_text(current.clone().unwrap_or_else(|| "None".to_owned()))
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "None").clicked() {
                    pick = Some(None);
                }
                for m in self.milestones.iter().flatten() {
                    if ui
                        .selectable_label(current.as_deref() == Some(m.title.as_str()), &m.title)
                        .clicked()
                    {
                        pick = Some(Some(m.title.clone()));
                    }
                }
            });
        if let Some(choice) = pick
            && choice != current
        {
            let args = match &choice {
                Some(t) => self.args(&["edit", "--milestone", t]),
                None => self.args(&["edit", "--remove-milestone"]),
            };
            self.quiet_args("Set milestone", args);
        }
    }

    /// Search box listing repository contributors and users with access;
    /// picking one requests their review.
    fn reviewer_picker(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        if self.people.is_none() && self.people_task.is_none() {
            self.load_people();
        }
        let edit = ui.add(
            egui::TextEdit::singleline(&mut self.reviewer_input)
                .hint_text("Request review: search people")
                .desired_width(f32::INFINITY),
        );
        if edit.gained_focus() || edit.changed() {
            self.reviewer_picker_open = true;
        }
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.reviewer_picker_open = false;
        }
        if !self.reviewer_picker_open {
            return;
        }

        // GitHub rejects the author, and already-requested people are listed above.
        let mut exclude: Vec<&str> = vec![d.author.login.as_str()];
        exclude.extend(d.review_requests.iter().map(|r| r.login.as_str()));
        let query = self
            .reviewer_input
            .trim()
            .trim_start_matches('@')
            .to_owned();
        let matches: Vec<model::Person> = self
            .people
            .as_deref()
            .map(|people| {
                model::search_people(people, &query, &exclude)
                    .into_iter()
                    .take(8)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let loading = self.people_task.is_some();
        // Allow any login GitHub might accept, even if not in our lists.
        let typed_login = !query.is_empty()
            && query.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !matches.iter().any(|p| p.login.eq_ignore_ascii_case(&query));

        let mut chosen = None;
        let list = egui::Frame::new()
            .fill(colors.surface.0)
            .stroke(egui::Stroke::new(1.0, colors.border.0))
            .corner_radius(6)
            .inner_margin(4)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 2.0;
                if loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Loading people…").small().weak());
                    });
                }
                for p in &matches {
                    let tip = if p.has_access {
                        "Request a review"
                    } else {
                        "Contributor; the request fails if they no longer have access"
                    };
                    let row = egui::Button::new(RichText::new(&p.login).strong())
                        .right_text(RichText::new(p.detail()).small().color(colors.text_muted.0))
                        .frame(false)
                        .truncate()
                        .min_size(egui::vec2(ui.available_width(), 0.0));
                    if ui.add(row).on_hover_text(tip).clicked() {
                        chosen = Some(p.login.clone());
                    }
                }
                if typed_login && ui.button(format!("Request review from @{query}")).clicked() {
                    chosen = Some(query.clone());
                }
                if matches.is_empty() && !typed_login && !loading {
                    ui.label(RichText::new("No matching people").small().weak());
                }
            });
        if enter && chosen.is_none() {
            chosen = matches
                .first()
                .map(|p| p.login.clone())
                .or_else(|| typed_login.then(|| query.clone()));
        }
        // Clicking anywhere else closes the list.
        let clicked_outside = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|pos| {
                    !edit.rect.contains(pos) && !list.response.rect.contains(pos)
                })
        });
        if clicked_outside {
            self.reviewer_picker_open = false;
        }
        if let Some(login) = chosen {
            self.reviewer_input.clear();
            self.reviewer_picker_open = false;
            let args = self.args(&["edit", "--add-reviewer", &login]);
            self.simple(&format!("Request review from {login}"), false, args);
        }
    }

    fn timeline(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        // Description.
        card(
            ui,
            colors,
            d.author.display(),
            &format!("opened {}", fmt::relative_iso(&d.created_at)),
            None,
            |ui| {
                let mut save = None;
                let mut cancel = false;
                if let Some(body) = &mut self.editing_body {
                    ui.add(
                        egui::TextEdit::multiline(body)
                            .desired_rows(10)
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            save = Some(body.clone());
                        }
                        cancel = ui.button("Cancel").clicked();
                    });
                    if cancel {
                        self.editing_body = None;
                    }
                } else {
                    markdown::show(ui, &d.body);
                    if ui.small_button("Edit description").clicked() {
                        self.editing_body = Some(d.body.clone());
                    }
                }
                if let Some(body) = save {
                    self.editing_body = None;
                    let n = self.number.to_string();
                    self.quiet("Edit description", move |cli| {
                        cli.run_with_input(&["pr", "edit", &n, "--body-file", "-"], &body)
                    });
                }
            },
        );

        enum Item<'a> {
            Comment(&'a model::IssueComment),
            Review(&'a model::Review),
        }
        let mut items: Vec<(i64, Item)> = Vec::new();
        for c in &d.comments {
            items.push((
                fmt::parse_timestamp(&c.created_at).unwrap_or(0),
                Item::Comment(c),
            ));
        }
        for r in &d.reviews {
            // Reviews whose only content is inline comments show as one line.
            items.push((
                fmt::parse_timestamp(&r.submitted_at).unwrap_or(0),
                Item::Review(r),
            ));
        }
        items.sort_by_key(|(t, _)| *t);
        for (_, item) in items {
            match item {
                Item::Comment(c) => {
                    card(
                        ui,
                        colors,
                        c.author.display(),
                        &format!("commented {}", fmt::relative_iso(&c.created_at)),
                        None,
                        |ui| {
                            markdown::show(ui, &c.body);
                        },
                    );
                }
                Item::Review(r) => {
                    let (verb, color) = match r.state.as_str() {
                        "APPROVED" => ("approved these changes", Some(colors.added.0)),
                        "CHANGES_REQUESTED" => ("requested changes", Some(colors.deleted.0)),
                        "DISMISSED" => ("review was dismissed", None),
                        "PENDING" => ("has a pending review", None),
                        _ => ("reviewed", None),
                    };
                    let when = format!("{verb} {}", fmt::relative_iso(&r.submitted_at));
                    if r.body.trim().is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            icons::label(
                                ui,
                                Icon::Check,
                                color.unwrap_or(colors.text_muted.0),
                                14.0,
                            );
                            ui.label(RichText::new(r.author.display()).strong());
                            ui.label(RichText::new(when).color(colors.text_muted.0));
                        });
                        ui.add_space(6.0);
                    } else {
                        card(ui, colors, r.author.display(), &when, color, |ui| {
                            markdown::show(ui, &r.body)
                        });
                    }
                }
            }
        }
        let inline = self.comments.len();
        if inline > 0
            && ui
                .link(format!(
                    "{inline} inline comment(s) — view in Files changed"
                ))
                .clicked()
        {
            self.tab = Tab::Files;
            if self.diff.is_none() {
                self.load_diff();
            }
        }

        ui.add_space(8.0);
        self.checks(ui, d, colors);
        ui.add_space(8.0);
        self.merge_box(ui, d, colors);
        ui.add_space(8.0);

        // New comment.
        ui.label(RichText::new("Add a comment").strong());
        ui.add(
            egui::TextEdit::multiline(&mut self.comment_draft)
                .hint_text("Leave a comment (Markdown)")
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            let ok = !self.comment_draft.trim().is_empty() && self.action.is_none();
            if ui
                .add_enabled(
                    ok,
                    egui::Button::new(RichText::new("Comment").color(colors.accent_text.0))
                        .fill(colors.accent.0),
                )
                .clicked()
            {
                self.submit_comment();
            }
            if d.state == "OPEN" && ui.button("Close pull request").clicked() {
                self.confirm = Some(Confirm::Close);
            }
            if d.state == "CLOSED" && ui.button("Reopen pull request").clicked() {
                let args = self.args(&["reopen"]);
                self.simple("Reopen", false, args);
            }
        });
        ui.add_space(20.0);
    }

    fn checks(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        if d.status_check_rollup.is_empty() {
            return;
        }
        let mut rerun = None;
        boxed(ui, colors, |ui| {
            let outcome = model::rollup(&d.status_check_rollup);
            if let Some(o) = outcome {
                let (glyph, color, text) = check_glyph(o, colors);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(glyph).color(color).strong());
                    ui.label(RichText::new(text).strong());
                    ui.label(
                        RichText::new(format!("{} check(s)", d.status_check_rollup.len()))
                            .color(colors.text_muted.0),
                    );
                });
            }
            ui.separator();
            for c in &d.status_check_rollup {
                ui.horizontal(|ui| {
                    let (glyph, color, _) = check_glyph(c.outcome(), colors);
                    ui.label(RichText::new(glyph).color(color));
                    ui.label(c.title());
                    if !c.description.is_empty() {
                        ui.label(
                            RichText::new(&c.description)
                                .small()
                                .color(colors.text_muted.0),
                        );
                    }
                    if !c.url().is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.hyperlink_to("Details", c.url());
                            if let Some((run, job)) = model::actions_job(c.url()) {
                                let failed = c.outcome() == model::CheckOutcome::Fail;
                                if failed
                                    && ui
                                        .small_button("Re-run failed")
                                        .on_hover_text(
                                            "Re-run the failed jobs of this workflow run",
                                        )
                                        .clicked()
                                {
                                    rerun = Some(run);
                                }
                                if ui
                                    .small_button(if failed { "Failed log" } else { "Log" })
                                    .on_hover_text("Show the job's log in a tab")
                                    .clicked()
                                {
                                    self.events.push(GhEvent::OpenLog {
                                        title: format!("Log: {}", c.title()),
                                        run,
                                        job,
                                        failed_only: failed,
                                    });
                                }
                            }
                        });
                    }
                });
            }
        });
        if let Some(run) = rerun {
            self.quiet("Re-run failed jobs", move |cli| {
                cli.run(&["run", "rerun", &run.to_string(), "--failed"])
            });
        }
    }

    fn merge_box(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        boxed(ui, colors, |ui| {
            match d.state.as_str() {
                "MERGED" => {
                    ui.horizontal(|ui| {
                        icons::label(ui, Icon::Merge, state_color("MERGED", false, colors), 16.0);
                        ui.label(
                            RichText::new(format!("Merged {}", fmt::relative_iso(&d.merged_at)))
                                .strong(),
                        );
                    });
                    return;
                }
                "CLOSED" => {
                    ui.label(RichText::new("This pull request is closed.").strong());
                    return;
                }
                _ => {}
            }
            if d.is_draft {
                ui.label(RichText::new("This pull request is still a draft.").strong());
                ui.label(
                    RichText::new("It can't be merged until it's marked ready for review.")
                        .color(colors.text_muted.0),
                );
                if ui.button("Ready for review").clicked() {
                    let args = self.args(&["ready"]);
                    self.simple("Mark ready", false, args);
                }
                return;
            }
            let (msg, color) = match (d.mergeable.as_str(), d.merge_state_status.as_str()) {
                ("CONFLICTING", _) | (_, "DIRTY") => (
                    "This branch has conflicts that must be resolved.",
                    colors.deleted.0,
                ),
                (_, "BLOCKED") => (
                    "Merging is blocked (required reviews or checks).",
                    colors.warning.0,
                ),
                (_, "BEHIND") => (
                    "This branch is out-of-date with the base branch.",
                    colors.warning.0,
                ),
                (_, "UNSTABLE") => ("Some checks are not passing.", colors.warning.0),
                (_, "CLEAN") => (
                    "This branch has no conflicts with the base branch.",
                    colors.added.0,
                ),
                _ => ("Checking mergeability…", colors.text_muted.0),
            };
            ui.label(RichText::new(msg).color(color).strong());
            ui.horizontal(|ui| {
                if d.merge_state_status == "BEHIND" && ui.button("Update branch").clicked() {
                    let args = self.args(&["update-branch"]);
                    self.simple("Update branch", false, args);
                }
                egui::ComboBox::from_id_salt(("merge_method", d.number))
                    .selected_text(self.merge_method.label())
                    .show_ui(ui, |ui| {
                        for m in [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase] {
                            ui.selectable_value(&mut self.merge_method, m, m.label());
                        }
                    });
                let can = d.mergeable != "CONFLICTING" && self.action.is_none();
                if ui
                    .add_enabled(
                        can,
                        egui::Button::new(RichText::new("Merge").color(Color32::WHITE))
                            .fill(colors.added.0.gamma_multiply(0.85)),
                    )
                    .clicked()
                {
                    self.confirm = Some(Confirm::Merge);
                }
                ui.checkbox(&mut self.delete_branch, "Delete branch");
            });
            ui.horizontal(|ui| {
                if ui.small_button("Convert to draft").clicked() {
                    let args = self.args(&["ready", "--undo"]);
                    self.simple("Convert to draft", false, args);
                }
            });
        });
    }

    fn commits(&mut self, ui: &mut Ui, d: &PrDetail, colors: &UiColors) {
        let mut open = None;
        for c in &d.commits {
            let expanded = self.commit.as_ref().is_some_and(|(oid, _)| *oid == c.oid);
            ui.horizontal(|ui| {
                icons::label(ui, Icon::Commit, colors.text_muted.0, 16.0);
                let r = ui
                    .add(
                        egui::Label::new(RichText::new(&c.message_headline).strong())
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text(if c.message_body.is_empty() {
                        "Show changes"
                    } else {
                        &c.message_body
                    });
                if r.clicked() {
                    open = Some(c.oid.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button(&c.oid[..c.oid.len().min(7)])
                        .on_hover_text("Copy full SHA")
                        .clicked()
                    {
                        ui.ctx().copy_text(c.oid.clone());
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} · {}",
                            c.author(),
                            fmt::relative_iso(&c.authored_date)
                        ))
                        .small()
                        .color(colors.text_muted.0),
                    );
                });
            });
            if expanded {
                match &self.commit {
                    Some((_, Some(Ok(files)))) => {
                        let id = ui.make_persistent_id(("pr_commit", &c.oid));
                        diff_view::show(
                            ui,
                            id,
                            files,
                            &DiffColors::from_theme(colors),
                            &mut diff_view::NoHooks,
                            None,
                        );
                    }
                    Some((_, Some(Err(e)))) => {
                        ui.colored_label(colors.error.0, e);
                    }
                    _ => {
                        ui.spinner();
                    }
                }
            }
            ui.separator();
        }
        if let Some(oid) = open {
            if self.commit.as_ref().is_some_and(|(o, _)| *o == oid) {
                self.commit = None;
            } else {
                self.load_commit(oid);
            }
        }
    }

    fn files(&mut self, ui: &mut Ui, colors: &UiColors) {
        let Some(files) = self.diff.clone() else {
            ui.add_space(12.0);
            match &self.diff_error {
                Some(e) => {
                    ui.colored_label(colors.error.0, e);
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading diff…");
                    });
                }
            }
            return;
        };
        let viewed = self
            .extra
            .as_ref()
            .map(|e| e.viewed.clone())
            .unwrap_or_default();
        let mut submit = None;
        egui::Panel::left(egui::Id::unique(("pr_file_list", self.number)))
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let seen = files.iter().filter(|f| viewed.contains(f.path())).count();
                    ui.label(
                        RichText::new(format!("{} files · {seen} viewed", files.len()))
                            .small()
                            .strong()
                            .color(colors.text_muted.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        diff_view::layout_toggle(ui);
                    });
                });
                egui::ScrollArea::vertical()
                    .id_salt(("pr_files_scroll", self.number))
                    .show(ui, |ui| {
                        for (i, f) in files.iter().enumerate() {
                            let threads = self
                                .threads
                                .by_line
                                .keys()
                                .filter(|(p, _, _)| p == f.path())
                                .count();
                            let is_viewed = viewed.contains(f.path());
                            ui.horizontal(|ui| {
                                let mut v = is_viewed;
                                if ui.checkbox(&mut v, "").on_hover_text("Viewed").changed()
                                    && self.extra.is_some()
                                {
                                    submit = Some(Submit::ToggleViewed(f.path().to_owned(), v));
                                }
                                let dc = DiffColors::from_theme(colors);
                                ui.label(
                                    RichText::new(diff_view::status_letter(f.status))
                                        .monospace()
                                        .color(diff_view::status_color(f.status, &dc)),
                                );
                                let name = f.path().rsplit('/').next().unwrap_or(f.path());
                                let text = if is_viewed {
                                    RichText::new(name).color(colors.text_muted.0)
                                } else {
                                    RichText::new(name)
                                };
                                if ui
                                    .add(
                                        egui::Label::new(text)
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text(f.path())
                                    .clicked()
                                {
                                    self.scroll_to_file = Some(i);
                                }
                                if threads > 0 {
                                    icons::label(ui, Icon::Comment, colors.text_muted.0, 12.0)
                                        .on_hover_text(format!("{threads} comment thread(s)"));
                                }
                            });
                        }
                    });
            });

        // GitHub accepts review comments on open, closed and merged PRs.
        let can_comment = true;
        let scroll_to = self.scroll_to_file.take();
        let can_apply = self.can_apply_suggestions();
        if !self.pending.is_empty() {
            egui::Frame::new()
                .fill(colors.warning.0.gamma_multiply(0.12))
                .inner_margin(egui::Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(format!(
                            "{} pending review comment(s). Submit them with “Finish review” above.",
                            self.pending.len()
                        ))
                        .color(colors.warning.0),
                    );
                });
        }
        egui::ScrollArea::both()
            .id_salt(("pr_diff", self.number))
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.add_space(6.0);
                let mut hooks = FilesHooks {
                    comments: &self.comments,
                    threads: &self.threads,
                    extra: self.extra.as_ref(),
                    pending: &self.pending,
                    expanded: &mut self.expanded_resolved,
                    inline: &mut self.inline,
                    reply: &mut self.reply,
                    submit: &mut submit,
                    colors,
                    can_comment,
                    can_apply,
                    viewed: &viewed,
                    busy: self.action.is_some(),
                };
                let id = ui.make_persistent_id(("pr_files", self.number));
                diff_view::show(
                    ui,
                    id,
                    &files,
                    &DiffColors::from_theme(colors),
                    &mut hooks,
                    scroll_to,
                );
            });
        match submit {
            Some(Submit::Inline(draft)) => self.submit_inline(draft),
            Some(Submit::Reply(root, body)) => self.submit_reply(root, body),
            Some(Submit::Pending(draft)) => self.pending.push(draft),
            Some(Submit::RemovePending(i)) => {
                if i < self.pending.len() {
                    self.pending.remove(i);
                }
            }
            Some(Submit::Resolve(thread, resolve)) => self.resolve_thread(thread, resolve),
            Some(Submit::ToggleViewed(path, v)) => self.set_viewed(path, v),
            Some(Submit::ApplySuggestion {
                path,
                start,
                end,
                text,
            }) => {
                self.apply_suggestion(&path, start, end, &text);
            }
            None => {}
        }
    }

    fn confirm_dialog(&mut self, ui: &mut Ui, d: &PrDetail) {
        let Some(confirm) = self.confirm else { return };
        let (title, body, button) = match confirm {
            Confirm::Merge => (
                "Merge Pull Request",
                format!(
                    "{} #{} into {}?{}",
                    self.merge_method.label(),
                    d.number,
                    d.base_ref_name,
                    if self.delete_branch {
                        "\nThe branch will be deleted."
                    } else {
                        ""
                    }
                ),
                "Confirm merge",
            ),
            Confirm::Close => (
                "Close Pull Request",
                format!("Close #{} without merging?", d.number),
                "Close",
            ),
        };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique(("pr_confirm", d.number))).show(ui.ctx(), |ui| {
            ui.set_max_width(380.0);
            ui.heading(title);
            ui.add_space(4.0);
            ui.label(body);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(button).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                self.confirm = None;
                match confirm {
                    Confirm::Merge => {
                        let mut args = self.args(&["merge", self.merge_method.flag()]);
                        if self.delete_branch {
                            args.push("--delete-branch".into());
                        }
                        // Deleting the branch may switch the local checkout.
                        let git_changed = self.delete_branch;
                        self.simple("Merge", git_changed, args);
                    }
                    Confirm::Close => {
                        let args = self.args(&["close"]);
                        self.simple("Close", false, args);
                    }
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

/// Inline comment threads, pending review comments, suggestions and the
/// comment composer inside the diff.
struct FilesHooks<'a> {
    comments: &'a [ReviewComment],
    threads: &'a Threads,
    extra: Option<&'a model::PrExtra>,
    pending: &'a [InlineDraft],
    expanded: &'a mut std::collections::HashSet<u64>,
    inline: &'a mut Option<InlineDraft>,
    reply: &'a mut Option<(u64, String)>,
    submit: &'a mut Option<Submit>,
    colors: &'a UiColors,
    can_comment: bool,
    can_apply: bool,
    viewed: &'a std::collections::HashSet<String>,
    busy: bool,
}

fn thread_frame(colors: &UiColors, stroke: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(colors.surface.0)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(6)
        .inner_margin(10)
        .outer_margin(egui::Margin {
            left: 60,
            right: 8,
            top: 4,
            bottom: 6,
        })
}

impl FilesHooks<'_> {
    fn thread(&mut self, ui: &mut Ui, root_idx: usize, outdated: bool, file: Option<&FileDiff>) {
        let root = &self.comments[root_idx];
        let colors = self.colors;
        // Wide diffs scroll horizontally; size cards to what's visible.
        let width = ui.clip_rect().width().min(760.0);
        let info = self.extra.and_then(|e| e.threads.get(&root.id)).cloned();
        let resolved = info.as_ref().is_some_and(|(_, r)| *r);
        let replies = self
            .threads
            .replies
            .get(&root.id)
            .cloned()
            .unwrap_or_default();
        if resolved && !self.expanded.contains(&root.id) {
            thread_frame(colors, colors.border.0).show(ui, |ui| {
                ui.set_width(width - 90.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✔ Resolved").color(colors.added.0).strong());
                    ui.label(
                        RichText::new(format!(
                            "{} · {} comment(s)",
                            root.user.login,
                            replies.len() + 1
                        ))
                        .small()
                        .color(colors.text_muted.0),
                    );
                    if ui.small_button("Show").clicked() {
                        self.expanded.insert(root.id);
                    }
                    if let Some((tid, _)) = &info
                        && ui.small_button("Unresolve").clicked()
                    {
                        *self.submit = Some(Submit::Resolve(tid.clone(), false));
                    }
                });
            });
            return;
        }
        thread_frame(colors, colors.border.0).show(ui, |ui| {
            ui.set_width(width - 90.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            if outdated {
                ui.label(
                    RichText::new(format!(
                        "Outdated · originally line {}",
                        root.original_line.map_or("?".to_owned(), |l| l.to_string())
                    ))
                    .small()
                    .color(colors.warning.0),
                );
            }
            for idx in std::iter::once(root_idx).chain(replies) {
                let c = &self.comments[idx];
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&c.user.login).strong());
                    ui.label(
                        RichText::new(fmt::relative_iso(&c.created_at))
                            .small()
                            .color(colors.text_muted.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !c.html_url.is_empty()
                            && icons::button_sized(ui, Icon::External, "Open on GitHub", 18.0)
                                .clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&c.html_url));
                        }
                    });
                });
                let (parts, tail) = model::split_suggestions(&c.body);
                for (before, suggestion) in &parts {
                    if !before.trim().is_empty() {
                        markdown::show(ui, before);
                    }
                    self.suggestion(ui, c, suggestion, file);
                }
                if !tail.trim().is_empty() || parts.is_empty() {
                    markdown::show(ui, &tail);
                }
                ui.add_space(2.0);
            }
            let replying = self.reply.as_ref().is_some_and(|(id, _)| *id == root.id);
            if replying {
                let mut send = false;
                let mut cancel = false;
                if let Some((_, body)) = self.reply.as_mut() {
                    ui.add(
                        egui::TextEdit::multiline(body)
                            .hint_text("Reply…")
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        send = ui
                            .add_enabled(
                                !body.trim().is_empty() && !self.busy,
                                egui::Button::new("Reply"),
                            )
                            .clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                }
                if send && let Some((id, body)) = self.reply.take() {
                    *self.submit = Some(Submit::Reply(id, body));
                }
                if cancel {
                    *self.reply = None;
                }
            } else {
                ui.horizontal(|ui| {
                    if ui.button("Reply…").clicked() {
                        *self.reply = Some((root.id, String::new()));
                    }
                    if let Some((tid, resolved)) = &info {
                        if !resolved && ui.button("Resolve conversation").clicked() {
                            *self.submit = Some(Submit::Resolve(tid.clone(), true));
                        }
                        if *resolved {
                            if ui.button("Unresolve").clicked() {
                                *self.submit = Some(Submit::Resolve(tid.clone(), false));
                            }
                            if ui.button("Hide").clicked() {
                                self.expanded.remove(&root.id);
                            }
                        }
                    }
                });
            }
        });
    }

    /// A ```suggestion block: the lines it replaces and the new ones.
    fn suggestion(&mut self, ui: &mut Ui, c: &ReviewComment, text: &str, file: Option<&FileDiff>) {
        let colors = self.colors;
        let end = c.line;
        let start = c.start_line.or(c.line);
        let original: Vec<String> = match (file, start, end) {
            (Some(f), Some(s), Some(e)) if c.side != "LEFT" => f
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| {
                    l.kind != LineKind::Removed && l.new_no.is_some_and(|n| n >= s && n <= e)
                })
                .map(|l| l.text.clone())
                .collect(),
            _ => Vec::new(),
        };
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, colors.border.0))
            .corner_radius(4)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Suggested change").small().strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let applicable = self.can_apply && start.is_some() && c.side != "LEFT";
                        let tip = if applicable {
                            "Replace these lines in your working copy"
                        } else {
                            "Check out this pull request's branch to apply suggestions"
                        };
                        if ui
                            .add_enabled(applicable, egui::Button::new("Apply suggestion").small())
                            .on_hover_text(tip)
                            .on_disabled_hover_text(tip)
                            .clicked()
                            && let (Some(s), Some(e)) = (start, end)
                        {
                            *self.submit = Some(Submit::ApplySuggestion {
                                path: c.path.clone(),
                                start: s,
                                end: e,
                                text: text.to_owned(),
                            });
                        }
                    });
                });
                let mono = |t: &str, sign: &str, bg: Color32, ui: &mut Ui| {
                    egui::Frame::new().fill(bg).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(format!("{sign} {t}")).monospace());
                    });
                };
                ui.spacing_mut().item_spacing.y = 0.0;
                for l in &original {
                    mono(l, "−", colors.deleted.0.gamma_multiply(0.16), ui);
                }
                for l in text.split('\n') {
                    mono(l, "+", colors.added.0.gamma_multiply(0.16), ui);
                }
            });
    }
}

impl DiffHooks for FilesHooks<'_> {
    fn can_click(&self, line: &DiffLine) -> bool {
        self.can_comment && line_anchor(line).is_some()
    }

    fn line_clicked(&mut self, _at: LineRef, file: &FileDiff, line: &DiffLine, _shift: bool) {
        if let Some((side, n)) = line_anchor(line) {
            let same = self
                .inline
                .as_ref()
                .is_some_and(|d| d.path == file.path() && d.side == side && d.line == n);
            if !same {
                *self.inline = Some(InlineDraft {
                    path: file.path().to_owned(),
                    side,
                    line: n,
                    body: String::new(),
                    line_text: line.text.trim_end_matches('\r').to_owned(),
                });
            }
        }
    }

    fn file_actions(&mut self, ui: &mut Ui, _idx: usize, file: &FileDiff) {
        if self.extra.is_none() {
            return;
        }
        let mut v = self.viewed.contains(file.path());
        if ui.checkbox(&mut v, "Viewed").changed() {
            *self.submit = Some(Submit::ToggleViewed(file.path().to_owned(), v));
        }
    }

    fn file_preamble(&mut self, ui: &mut Ui, file: &FileDiff) {
        if let Some(roots) = self.threads.outdated.get(file.path()).cloned() {
            ui.collapsing(
                RichText::new(format!("{} outdated comment thread(s)", roots.len()))
                    .color(self.colors.warning.0),
                |ui| {
                    for idx in roots {
                        self.thread(ui, idx, true, None);
                    }
                },
            );
        }
    }

    fn after_line(&mut self, ui: &mut Ui, file: &FileDiff, line: &DiffLine) {
        let Some((side, n)) = line_anchor(line) else {
            return;
        };
        let colors = self.colors;
        if let Some(roots) = self
            .threads
            .by_line
            .get(&(file.path().to_owned(), side, n))
            .cloned()
        {
            for idx in roots {
                self.thread(ui, idx, false, Some(file));
            }
        }
        // Pending review comments on this line.
        // Wide diffs scroll horizontally; size cards to what's visible.
        let width = ui.clip_rect().width().min(760.0);
        for (i, p) in self.pending.iter().enumerate() {
            if p.path != file.path() || p.side != side || p.line != n {
                continue;
            }
            thread_frame(colors, colors.warning.0).show(ui, |ui| {
                ui.set_width(width - 90.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Pending")
                            .small()
                            .strong()
                            .color(colors.warning.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Delete").clicked() {
                            *self.submit = Some(Submit::RemovePending(i));
                        }
                    });
                });
                markdown::show(ui, &p.body);
            });
        }
        let editing = self
            .inline
            .as_ref()
            .is_some_and(|d| d.path == file.path() && d.side == side && d.line == n);
        if !editing {
            return;
        }
        let mut action: Option<u8> = None;
        thread_frame(colors, colors.accent.0).show(ui, |ui| {
            ui.set_width(width - 90.0);
            if let Some(draft) = self.inline.as_mut() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Comment on line {} ({})",
                            n,
                            if side == Side::Left { "old" } else { "new" }
                        ))
                        .small()
                        .color(colors.text_muted.0),
                    );
                    if side == Side::Right
                        && ui
                            .small_button("Suggest change")
                            .on_hover_text("Propose replacement code the author can apply")
                            .clicked()
                    {
                        draft
                            .body
                            .push_str(&format!("```suggestion\n{}\n```\n", draft.line_text));
                    }
                });
                let edit = ui.add(
                    egui::TextEdit::multiline(&mut draft.body)
                        .hint_text("Leave a comment (Markdown)")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if draft.body.is_empty() && !edit.has_focus() {
                    edit.request_focus();
                }
                let ok = !draft.body.trim().is_empty();
                ui.horizontal(|ui| {
                    let label = if self.pending.is_empty() {
                        "Start a review"
                    } else {
                        "Add review comment"
                    };
                    if ui
                        .add_enabled(
                            ok,
                            egui::Button::new(RichText::new(label).color(colors.accent_text.0))
                                .fill(colors.accent.0),
                        )
                        .on_hover_text(
                            "Saved as pending; submitted together when you finish your review",
                        )
                        .clicked()
                    {
                        action = Some(1);
                    }
                    if ui
                        .add_enabled(ok && !self.busy, egui::Button::new("Add single comment"))
                        .on_hover_text("Post this comment right away")
                        .clicked()
                    {
                        action = Some(0);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(2);
                    }
                });
            }
        });
        match action {
            Some(0) => {
                if let Some(d) = self.inline.take() {
                    *self.submit = Some(Submit::Inline(d));
                }
            }
            Some(1) => {
                if let Some(d) = self.inline.take() {
                    *self.submit = Some(Submit::Pending(d));
                }
            }
            Some(_) => *self.inline = None,
            None => {}
        }
    }
}

/// `content` with 1-based lines `start..=end` replaced by `text`.
fn replace_lines(content: &str, start: u32, end: u32, text: &str) -> Result<String, String> {
    let mut lines: Vec<&str> = content.split('\n').collect();
    let (s, e) = ((start as usize).saturating_sub(1), end as usize);
    if start == 0 || s >= e || e > lines.len() {
        return Err("the file no longer has those lines; pull the latest changes".to_owned());
    }
    lines.splice(s..e, text.split('\n'));
    Ok(lines.join("\n"))
}

/// A timeline card: author, action, then content.
fn card(
    ui: &mut Ui,
    colors: &UiColors,
    author: &str,
    what: &str,
    accent: Option<Color32>,
    body: impl FnOnce(&mut Ui),
) {
    egui::Frame::new()
        .fill(colors.surface.0)
        .stroke(egui::Stroke::new(1.0, accent.unwrap_or(colors.border.0)))
        .corner_radius(6)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Frame::new()
                .fill(colors.widget.0.gamma_multiply(0.5))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(author).strong());
                        ui.label(RichText::new(what).color(colors.text_muted.0));
                    });
                });
            egui::Frame::new().inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                body(ui);
            });
        });
    ui.add_space(10.0);
}

fn boxed(ui: &mut Ui, colors: &UiColors, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(colors.surface.0)
        .stroke(egui::Stroke::new(1.0, colors.border.0))
        .corner_radius(6)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_replace_line_ranges() {
        let content = "a\nb\nc\nd\n";
        assert_eq!(replace_lines(content, 2, 2, "B").unwrap(), "a\nB\nc\nd\n");
        assert_eq!(
            replace_lines(content, 2, 3, "x\ny\nz").unwrap(),
            "a\nx\ny\nz\nd\n"
        );
        assert_eq!(replace_lines(content, 1, 1, "").unwrap(), "\nb\nc\nd\n");
        assert!(replace_lines(content, 9, 9, "x").is_err());
        assert!(replace_lines(content, 0, 1, "x").is_err());
    }
}

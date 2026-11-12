//! The pull request view, shown as a tab in the editor area: conversation,
//! commits and changed files, with reviewing, commenting (including inline
//! comments on diff lines), merging and checkout.

use std::collections::HashMap;

use egui::{Color32, RichText, Ui};

use super::GhEvent;
use super::list::{check_glyph, label_chip, review_badge, state_color, state_label};
use super::model::{self, PrDetail, ReviewComment};
use crate::cli::Cli;
use crate::git::diff::{self, DiffLine, FileDiff};
use crate::task::{self, Pending};
use crate::theme::UiColors;
use crate::ui::diff_view::{self, DiffColors, DiffHooks, Side, line_anchor};
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
}

enum Submit {
    Inline(InlineDraft),
    Reply(u64, String),
}

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
    merge_method: MergeMethod,
    delete_branch: bool,
    confirm: Option<Confirm>,
    scroll_to_file: Option<usize>,
    /// Running action: label, whether it changes local git state, result.
    action: Option<Action>,
    events: Vec<GhEvent>,
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
            merge_method: MergeMethod::Squash,
            delete_branch: false,
            confirm: None,
            scroll_to_file: None,
            action: None,
            events: Vec::new(),
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
        if self.diff.is_some() || self.tab == Tab::Files {
            self.load_diff();
        }
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
                Ok(_) => self.events.push(GhEvent::Done(format!("{label}: done"))),
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
        ui.horizontal(|ui| {
            ui.label(RichText::new(&d.title).size(19.0).strong());
            ui.label(
                RichText::new(format!("#{}", d.number))
                    .size(19.0)
                    .color(colors.text_muted.0),
            );
        });
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
        let button = ui.add(
            egui::Button::new(RichText::new("Review changes ▾").color(colors.accent_text.0))
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
                let needs_body = self.review_kind != ReviewKind::Approve;
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
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.reviewer_input)
                        .hint_text("Request review: login")
                        .desired_width(170.0),
                );
                let login = self
                    .reviewer_input
                    .trim()
                    .trim_start_matches('@')
                    .to_owned();
                if ui
                    .add_enabled(!login.is_empty(), egui::Button::new("Add").small())
                    .clicked()
                {
                    self.reviewer_input.clear();
                    let args = self.args(&["edit", "--add-reviewer", &login]);
                    self.simple("Request review", false, args);
                }
            });
        }
        if let Some((text, color)) = review_badge(&d.review_decision, colors) {
            ui.label(RichText::new(text).small().color(color));
        }
        ui.add_space(8.0);
        heading(ui, "Assignees");
        if d.assignees.is_empty() {
            ui.label(RichText::new("No one").weak().small());
        }
        for a in &d.assignees {
            ui.label(a.display());
        }
        ui.add_space(8.0);
        heading(ui, "Labels");
        if d.labels.is_empty() {
            ui.label(RichText::new("None yet").weak().small());
        }
        ui.horizontal_wrapped(|ui| {
            for l in &d.labels {
                label_chip(ui, &l.name, &l.color);
            }
        });
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
                markdown::show(ui, &d.body);
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
                        });
                    }
                });
            }
        });
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
        egui::Panel::left(egui::Id::unique(("pr_file_list", self.number)))
            .resizable(true)
            .default_size(230.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("{} files", files.len()))
                        .small()
                        .strong()
                        .color(colors.text_muted.0),
                );
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
                            ui.horizontal(|ui| {
                                let dc = DiffColors::from_theme(colors);
                                ui.label(
                                    RichText::new(diff_view::status_letter(f.status))
                                        .monospace()
                                        .color(diff_view::status_color(f.status, &dc)),
                                );
                                let name = f.path().rsplit('/').next().unwrap_or(f.path());
                                if ui
                                    .add(
                                        egui::Label::new(name)
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
        let mut submit = None;
        let scroll_to = self.scroll_to_file.take();
        egui::ScrollArea::both()
            .id_salt(("pr_diff", self.number))
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.add_space(6.0);
                let mut hooks = FilesHooks {
                    comments: &self.comments,
                    threads: &self.threads,
                    inline: &mut self.inline,
                    reply: &mut self.reply,
                    submit: &mut submit,
                    colors,
                    can_comment,
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

/// Inline comment threads and the comment composer inside the diff.
struct FilesHooks<'a> {
    comments: &'a [ReviewComment],
    threads: &'a Threads,
    inline: &'a mut Option<InlineDraft>,
    reply: &'a mut Option<(u64, String)>,
    submit: &'a mut Option<Submit>,
    colors: &'a UiColors,
    can_comment: bool,
    busy: bool,
}

impl FilesHooks<'_> {
    fn thread(&mut self, ui: &mut Ui, root_idx: usize, outdated: bool) {
        let root = &self.comments[root_idx];
        let colors = self.colors;
        let width = ui.available_width().min(760.0);
        egui::Frame::new()
            .fill(colors.surface.0)
            .stroke(egui::Stroke::new(1.0, colors.border.0))
            .corner_radius(6)
            .inner_margin(10)
            .outer_margin(egui::Margin {
                left: 60,
                right: 8,
                top: 4,
                bottom: 6,
            })
            .show(ui, |ui| {
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
                let replies = self
                    .threads
                    .replies
                    .get(&root.id)
                    .cloned()
                    .unwrap_or_default();
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
                    markdown::show(ui, &c.body);
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
                } else if ui.button("Reply…").clicked() {
                    *self.reply = Some((root.id, String::new()));
                }
            });
    }
}

impl DiffHooks for FilesHooks<'_> {
    fn lines_clickable(&self) -> bool {
        self.can_comment
    }

    fn line_clicked(&mut self, file: &FileDiff, line: &DiffLine) {
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
                });
            }
        }
    }

    fn file_preamble(&mut self, ui: &mut Ui, file: &FileDiff) {
        if let Some(roots) = self.threads.outdated.get(file.path()).cloned() {
            ui.collapsing(
                RichText::new(format!("{} outdated comment thread(s)", roots.len()))
                    .color(self.colors.warning.0),
                |ui| {
                    for idx in roots {
                        self.thread(ui, idx, true);
                    }
                },
            );
        }
    }

    fn after_line(&mut self, ui: &mut Ui, file: &FileDiff, line: &DiffLine) {
        let Some((side, n)) = line_anchor(line) else {
            return;
        };
        if let Some(roots) = self
            .threads
            .by_line
            .get(&(file.path().to_owned(), side, n))
            .cloned()
        {
            for idx in roots {
                self.thread(ui, idx, false);
            }
        }
        let editing = self
            .inline
            .as_ref()
            .is_some_and(|d| d.path == file.path() && d.side == side && d.line == n);
        if !editing {
            return;
        }
        let colors = self.colors;
        let width = ui.available_width().min(760.0);
        let mut send = false;
        let mut cancel = false;
        egui::Frame::new()
            .fill(colors.surface.0)
            .stroke(egui::Stroke::new(1.0, colors.accent.0))
            .corner_radius(6)
            .inner_margin(10)
            .outer_margin(egui::Margin {
                left: 60,
                right: 8,
                top: 4,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.set_width(width - 90.0);
                if let Some(draft) = self.inline.as_mut() {
                    ui.label(
                        RichText::new(format!(
                            "Comment on line {} ({})",
                            n,
                            if side == Side::Left { "old" } else { "new" }
                        ))
                        .small()
                        .color(colors.text_muted.0),
                    );
                    let edit = ui.add(
                        egui::TextEdit::multiline(&mut draft.body)
                            .hint_text("Leave a comment (Markdown)")
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    if draft.body.is_empty() && !edit.has_focus() {
                        edit.request_focus();
                    }
                    ui.horizontal(|ui| {
                        send = ui
                            .add_enabled(
                                !draft.body.trim().is_empty() && !self.busy,
                                egui::Button::new("Add comment"),
                            )
                            .on_hover_text("Posts immediately as a single comment")
                            .clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                }
            });
        if send && let Some(draft) = self.inline.take() {
            *self.submit = Some(Submit::Inline(draft));
        }
        if cancel {
            *self.inline = None;
        }
    }
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

//! One-click git: commit (with an AI-written message), push, and open a pull
//! request (with AI-written text), run as a single background job.

use std::sync::{Arc, Mutex};

use crate::cli::Cli;
use crate::git::suggest::PrText;
use crate::task::Pending;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKind {
    Commit,
    CommitPush,
    CommitPushPr,
    Push,
    PushPr,
    CreatePr,
}

impl FlowKind {
    pub fn label(self) -> &'static str {
        match self {
            FlowKind::Commit => "Commit",
            FlowKind::CommitPush => "Commit & push",
            FlowKind::CommitPushPr => "Commit, push & create PR",
            FlowKind::Push => "Push",
            FlowKind::PushPr => "Push & create PR",
            FlowKind::CreatePr => "Create PR",
        }
    }

    pub fn commits(self) -> bool {
        matches!(
            self,
            FlowKind::Commit | FlowKind::CommitPush | FlowKind::CommitPushPr
        )
    }

    pub fn pushes(self) -> bool {
        matches!(
            self,
            FlowKind::CommitPush | FlowKind::CommitPushPr | FlowKind::Push | FlowKind::PushPr
        )
    }

    pub fn opens_pr(self) -> bool {
        matches!(
            self,
            FlowKind::CommitPushPr | FlowKind::PushPr | FlowKind::CreatePr
        )
    }
}

pub type CommitJob = Box<dyn FnOnce() -> Result<String, String> + Send>;
pub type PrJob = Box<dyn FnOnce() -> Result<PrText, String> + Send>;

/// Everything a flow needs, gathered on the UI thread.
pub struct FlowPlan {
    pub kind: FlowKind,
    pub git: Cli,
    pub gh: Option<Cli>,
    /// Writes the commit message.
    pub commit_message: Option<CommitJob>,
    /// Nothing is staged, so commit every change.
    pub stage_all: bool,
    /// Push with `-u <remote> <branch>` (the branch has no upstream).
    pub publish_to: Option<String>,
    pub branch: String,
    pub base: String,
    /// Writes the PR title and description.
    pub pr_text: Option<PrJob>,
}

pub struct FlowDone {
    pub summary: String,
    pub pr_url: Option<String>,
}

pub struct GitFlow {
    pub kind: FlowKind,
    progress: Arc<Mutex<String>>,
    task: Pending<Result<FlowDone, String>>,
}

impl GitFlow {
    pub fn start(ctx: &egui::Context, plan: FlowPlan) -> Self {
        let kind = plan.kind;
        let progress = Arc::new(Mutex::new("Starting…".to_owned()));
        let p = progress.clone();
        let repaint = ctx.clone();
        let task = Pending::spawn(ctx, move || {
            let step = |s: &str| {
                *p.lock().unwrap() = s.to_owned();
                repaint.request_repaint();
            };
            run(plan, &step)
        });
        Self {
            kind,
            progress,
            task,
        }
    }

    pub fn progress(&self) -> String {
        self.progress.lock().unwrap().clone()
    }

    pub fn poll(&mut self) -> Option<Result<FlowDone, String>> {
        self.task.try_take()
    }
}

fn run(plan: FlowPlan, step: &dyn Fn(&str)) -> Result<FlowDone, String> {
    let git = &plan.git;
    let mut done = Vec::new();
    if plan.kind.commits() {
        step("Writing commit message…");
        let job = plan
            .commit_message
            .ok_or("there are no changes to commit")?;
        let message = job()?;
        if message.trim().is_empty() {
            return Err("the commit message came back empty".to_owned());
        }
        step("Committing…");
        if plan.stage_all {
            git.run(&["add", "-A"])?;
        }
        git.run_with_input(&["commit", "--cleanup=strip", "-F", "-"], &message)?;
        let subject = message.lines().next().unwrap_or_default().to_owned();
        done.push(format!("Committed “{subject}”"));
    }
    if plan.kind.pushes() {
        step("Pushing…");
        let mut args = vec!["push".to_owned()];
        if let Some(remote) = &plan.publish_to {
            args.extend(["-u".to_owned(), remote.clone(), plan.branch.clone()]);
        }
        let out = git.output(&args, None)?;
        if !out.success {
            return Err(out.into_result().unwrap_err());
        }
        done.push("pushed".to_owned());
    }
    let mut pr_url = None;
    if plan.kind.opens_pr() {
        let gh = plan
            .gh
            .as_ref()
            .ok_or("GitHub isn't connected (install gh and run `gh auth login`)")?;
        step("Writing pull request…");
        let job = plan.pr_text.ok_or("couldn't describe the branch")?;
        let text = job()?;
        step("Creating pull request…");
        let mut args = vec!["pr", "create", "--head", &plan.branch];
        if !plan.base.is_empty() {
            args.extend(["--base", &plan.base]);
        }
        args.extend(["--title", text.title.trim(), "--body-file", "-"]);
        let out = gh.run_with_input(&args, &text.body)?;
        let url = out
            .lines()
            .rev()
            .find(|l| l.starts_with("http"))
            .map(str::to_owned);
        done.push(format!("opened “{}”", text.title.trim()));
        pr_url = url;
    }
    let mut summary = done.join(", ");
    if let Some(first) = summary.get(..1) {
        summary = first.to_uppercase() + &summary[1..];
    }
    Ok(FlowDone { summary, pr_url })
}

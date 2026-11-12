//! GitHub integration through the GitHub CLI (`gh`): pull request lists,
//! details, reviews and comments for the repository open in the IDE.
//!
//! `gh` handles authentication and works out which GitHub repository the
//! folder belongs to from its git remotes, so Forge never stores tokens.

mod list;
pub mod model;
mod pr;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::Cli;
use crate::task::{self, Pending};
pub use list::{BranchContext, ListAction, PrListPanel, check_glyph};
use model::{Notification, PrSummary, RepoInfo, WorkflowRun};
pub use pr::PrPane;

/// How often the PR list refreshes while visible and focused.
const LIST_REFRESH: Duration = Duration::from_secs(90);
/// How often notifications are checked (for the activity bar badge).
const NOTIFICATION_REFRESH: Duration = Duration::from_secs(60);
/// How often the current branch's CI runs are checked, while some are
/// running and when all are finished.
const RUNS_REFRESH_ACTIVE: Duration = Duration::from_secs(15);
const RUNS_REFRESH_IDLE: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    NoFolder,
    Connecting,
    GhMissing,
    /// Not signed in; carries gh's explanation.
    LoggedOut(String),
    /// The folder has no GitHub remote (or isn't a repository).
    NoRepo(String),
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
    All,
}

impl PrState {
    pub const ALL: [PrState; 4] = [Self::Open, Self::Merged, Self::Closed, Self::All];

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Closed => "Closed",
            Self::Merged => "Merged",
            Self::All => "All",
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrFilter {
    Everyone,
    Mine,
    ReviewRequested,
    Assigned,
}

impl PrFilter {
    pub const ALL: [PrFilter; 4] = [
        Self::Everyone,
        Self::Mine,
        Self::ReviewRequested,
        Self::Assigned,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Everyone => "Everyone",
            Self::Mine => "Created by me",
            Self::ReviewRequested => "Review requested",
            Self::Assigned => "Assigned to me",
        }
    }
}

/// Something the app should react to.
#[derive(Debug, Clone)]
pub enum GhEvent {
    Done(String),
    Failed(String),
    /// Open the detail view for this PR.
    OpenPr(u64),
    /// Local git state changed (e.g. a PR was checked out).
    GitChanged,
    /// PR data changed; refresh the list.
    PrsChanged,
    /// Show a GitHub Actions log in a tab.
    OpenLog {
        title: String,
        run: u64,
        job: Option<u64>,
        failed_only: bool,
    },
}

/// Options for creating a pull request.
#[derive(Debug, Clone, Default)]
pub struct NewPr {
    pub title: String,
    pub body: String,
    pub base: String,
    pub head: String,
    pub draft: bool,
    /// Push the branch first (it has no upstream yet).
    pub push_to: Option<String>,
}

pub struct GitHub {
    ctx: egui::Context,
    gh_path: Option<PathBuf>,
    folder: Option<PathBuf>,
    pub connection: Connection,
    connect_task: Option<Pending<Result<(String, RepoInfo), Connection>>>,
    cli: Option<Cli>,
    /// Signed-in user.
    pub user: String,
    pub repo: RepoInfo,

    pub state: PrState,
    pub filter: PrFilter,
    pub query: String,
    pub prs: Vec<PrSummary>,
    list_task: Option<Pending<Result<Vec<PrSummary>, String>>>,
    pub list_error: Option<String>,
    last_list: Option<Instant>,

    create_task: Option<Pending<Result<String, String>>>,
    events: Vec<GhEvent>,

    /// Unread notifications for this repository.
    pub notifications: Vec<Notification>,
    notifications_task: Option<Pending<Result<Vec<Notification>, String>>>,
    last_notifications: Option<Instant>,

    /// CI runs for the commit `runs_commit`, newest first.
    pub runs: Vec<WorkflowRun>,
    runs_commit: Option<String>,
    runs_task: Option<Pending<Result<Vec<WorkflowRun>, String>>>,
    last_runs: Option<Instant>,
    /// Background fire-and-forget commands (mark read, re-run).
    side_tasks: Vec<(String, Pending<Result<String, String>>)>,
}

impl GitHub {
    pub fn new(ctx: egui::Context, folder: Option<&Path>) -> Self {
        let mut gh = Self {
            ctx,
            gh_path: Cli::locate("gh"),
            folder: None,
            connection: Connection::NoFolder,
            connect_task: None,
            cli: None,
            user: String::new(),
            repo: RepoInfo::default(),
            state: PrState::Open,
            filter: PrFilter::Everyone,
            query: String::new(),
            prs: Vec::new(),
            list_task: None,
            list_error: None,
            last_list: None,
            create_task: None,
            events: Vec::new(),
            notifications: Vec::new(),
            notifications_task: None,
            last_notifications: None,
            runs: Vec::new(),
            runs_commit: None,
            runs_task: None,
            last_runs: None,
            side_tasks: Vec::new(),
        };
        gh.open(folder);
        gh
    }

    /// A `gh` invocation bound to `dir`, never prompting.
    fn make_cli(gh: PathBuf, dir: PathBuf) -> Cli {
        Cli::new(gh, dir)
            .with_env("GH_PROMPT_DISABLED", "1")
            .with_env("GH_NO_UPDATE_NOTIFIER", "1")
            .with_env("GH_SPINNER_DISABLED", "1")
            .with_env("NO_COLOR", "1")
            .with_env("CLICOLOR", "0")
    }

    /// Connects to the GitHub repository for `folder`.
    pub fn open(&mut self, folder: Option<&Path>) {
        self.folder = folder.map(Path::to_path_buf);
        self.cli = None;
        self.prs.clear();
        self.list_error = None;
        self.last_list = None;
        self.notifications.clear();
        self.last_notifications = None;
        self.runs.clear();
        self.runs_commit = None;
        self.last_runs = None;
        self.connect();
    }

    /// (Re)checks the gh installation, sign-in and repository.
    pub fn connect(&mut self) {
        let Some(folder) = self.folder.clone() else {
            self.connection = Connection::NoFolder;
            return;
        };
        // Re-detect in case gh was installed while the IDE was running.
        if self.gh_path.is_none() {
            self.gh_path = Cli::locate("gh");
        }
        let Some(gh) = self.gh_path.clone() else {
            self.connection = Connection::GhMissing;
            return;
        };
        self.connection = Connection::Connecting;
        let cli = Self::make_cli(gh, folder);
        self.connect_task = Some(Pending::spawn(&self.ctx, move || {
            let auth = cli
                .output(&["auth", "status"], None)
                .map_err(Connection::LoggedOut)?;
            if !auth.success {
                let msg = if auth.stderr.trim().is_empty() {
                    auth.stdout
                } else {
                    auth.stderr
                };
                return Err(Connection::LoggedOut(msg.trim().to_owned()));
            }
            let user = cli
                .run(&["api", "user", "--jq", ".login"])
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            let repo = cli
                .run(&[
                    "repo",
                    "view",
                    "--json",
                    "nameWithOwner,url,defaultBranchRef",
                ])
                .map_err(Connection::NoRepo)?;
            let repo: RepoInfo = serde_json::from_str(&repo)
                .map_err(|e| Connection::NoRepo(format!("unexpected response from gh: {e}")))?;
            Ok((user, repo))
        }));
    }

    pub fn is_ready(&self) -> bool {
        self.connection == Connection::Ready
    }

    pub fn is_loading(&self) -> bool {
        self.list_task.is_some() || self.connect_task.is_some()
    }

    pub fn is_creating(&self) -> bool {
        self.create_task.is_some()
    }

    pub fn take_events(&mut self) -> Vec<GhEvent> {
        std::mem::take(&mut self.events)
    }

    /// Reloads the PR list with the current state, filter and query.
    pub fn refresh(&mut self) {
        let Some(cli) = self.cli.clone() else { return };
        self.last_list = Some(Instant::now());
        let mut args: Vec<String> = [
            "pr",
            "list",
            "--limit",
            "100",
            "--json",
            model::LIST_FIELDS,
            "--state",
            self.state.arg(),
        ]
        .map(String::from)
        .to_vec();
        let mut search = Vec::new();
        match self.filter {
            PrFilter::Everyone => {}
            PrFilter::Mine => args.extend(["--author".into(), "@me".into()]),
            PrFilter::Assigned => args.extend(["--assignee".into(), "@me".into()]),
            PrFilter::ReviewRequested => search.push("review-requested:@me".to_owned()),
        }
        if !self.query.trim().is_empty() {
            search.push(self.query.trim().to_owned());
        }
        if !search.is_empty() {
            args.extend(["--search".into(), search.join(" ")]);
        }
        self.list_task = Some(Pending::spawn(&self.ctx, move || {
            let out = cli.run(&args)?;
            serde_json::from_str(&out).map_err(|e| format!("unexpected response from gh: {e}"))
        }));
    }

    /// Picks up finished work; refreshes the list periodically while
    /// `visible`. Call once per frame.
    pub fn poll(&mut self, visible: bool) {
        if let Some(result) = task::poll(&mut self.connect_task) {
            match result {
                Ok((user, repo)) => {
                    self.user = user;
                    self.repo = repo;
                    self.connection = Connection::Ready;
                    if let (Some(gh), Some(folder)) = (self.gh_path.clone(), self.folder.clone()) {
                        self.cli = Some(Self::make_cli(gh, folder));
                    }
                    self.refresh();
                }
                Err(state) => self.connection = state,
            }
        }
        if let Some(result) = task::poll(&mut self.list_task) {
            match result {
                Ok(prs) => {
                    self.prs = prs;
                    self.list_error = None;
                }
                Err(e) => self.list_error = Some(e),
            }
        }
        if let Some(result) = task::poll(&mut self.create_task) {
            match result {
                Ok(url) => {
                    let url = url.trim().lines().last().unwrap_or_default().to_owned();
                    self.events
                        .push(GhEvent::Done(format!("Created pull request {url}")));
                    self.events.push(GhEvent::GitChanged);
                    if let Some(n) = url.rsplit('/').next().and_then(|n| n.parse().ok()) {
                        self.events.push(GhEvent::OpenPr(n));
                    }
                    self.refresh();
                }
                Err(e) => self
                    .events
                    .push(GhEvent::Failed(format!("Create pull request failed: {e}"))),
            }
        }
        if let Some(result) = task::poll(&mut self.notifications_task)
            && let Ok(all) = result
        {
            let repo = self.repo.name_with_owner.to_lowercase();
            self.notifications = all
                .into_iter()
                .filter(|n| n.unread && n.repository.full_name.to_lowercase() == repo)
                .collect();
        }
        if let Some(result) = task::poll(&mut self.runs_task)
            && let Ok(runs) = result
        {
            self.runs = runs;
        }
        let mut finished = Vec::new();
        self.side_tasks.retain(|(label, t)| match t.try_take() {
            Some(r) => {
                finished.push((label.clone(), r));
                false
            }
            None => true,
        });
        for (label, r) in finished {
            match r {
                Ok(_) if label.is_empty() => {}
                Ok(_) => self.events.push(GhEvent::Done(format!("{label}: done"))),
                Err(e) => self
                    .events
                    .push(GhEvent::Failed(format!("{label} failed: {e}"))),
            }
            self.last_runs = None;
        }
        if self.is_ready() && self.ctx.input(|i| i.focused) {
            if self.notifications_task.is_none()
                && self
                    .last_notifications
                    .is_none_or(|t| t.elapsed() >= NOTIFICATION_REFRESH)
            {
                self.refresh_notifications();
            }
            let busy_runs = self.runs.iter().any(|r| r.status != "completed");
            let every = if busy_runs {
                RUNS_REFRESH_ACTIVE
            } else {
                RUNS_REFRESH_IDLE
            };
            if self.runs_task.is_none()
                && self.runs_commit.is_some()
                && self.last_runs.is_none_or(|t| t.elapsed() >= every)
            {
                self.refresh_runs();
            }
            self.ctx.request_repaint_after(RUNS_REFRESH_ACTIVE);
        }
        if visible && self.is_ready() && self.list_task.is_none() {
            let focused = self.ctx.input(|i| i.focused);
            if focused && self.last_list.is_none_or(|t| t.elapsed() >= LIST_REFRESH) {
                self.refresh();
            }
            self.ctx.request_repaint_after(LIST_REFRESH);
        }
    }

    pub fn refresh_notifications(&mut self) {
        let Some(cli) = self.cli.clone() else { return };
        self.last_notifications = Some(Instant::now());
        self.notifications_task = Some(Pending::spawn(&self.ctx, move || {
            let out = cli.run(&["api", "notifications?per_page=50"])?;
            serde_json::from_str(&out).map_err(|e| format!("unexpected response from gh: {e}"))
        }));
    }

    /// Unread pull request notifications (the activity bar badge).
    pub fn badge_count(&self) -> usize {
        self.notifications
            .iter()
            .filter(|n| n.is_pull_request())
            .count()
    }

    pub fn mark_read(&mut self, id: &str) {
        let Some(cli) = self.cli.clone() else { return };
        self.notifications.retain(|n| n.id != id);
        let path = format!("notifications/threads/{id}");
        let task = Pending::spawn(&self.ctx, move || cli.run(&["api", "-X", "PATCH", &path]));
        self.side_tasks.push((String::new(), task));
    }

    pub fn mark_all_read(&mut self) {
        let Some(cli) = self.cli.clone() else { return };
        self.notifications.clear();
        let task = Pending::spawn(&self.ctx, move || {
            cli.run(&["api", "-X", "PUT", "repos/{owner}/{repo}/notifications"])
        });
        self.side_tasks
            .push(("Mark notifications read".into(), task));
    }

    /// Tracks CI runs for `commit` (the checked-out commit).
    pub fn watch_commit(&mut self, commit: Option<String>) {
        if commit != self.runs_commit {
            self.runs_commit = commit;
            self.runs.clear();
            self.last_runs = None;
        }
    }

    pub fn refresh_runs(&mut self) {
        let (Some(cli), Some(sha)) = (self.cli.clone(), self.runs_commit.clone()) else {
            return;
        };
        self.last_runs = Some(Instant::now());
        self.runs_task = Some(Pending::spawn(&self.ctx, move || {
            let out = cli.run(&[
                "run",
                "list",
                "--commit",
                &sha,
                "--limit",
                "20",
                "--json",
                model::RUN_FIELDS,
            ])?;
            serde_json::from_str(&out).map_err(|e| format!("unexpected response from gh: {e}"))
        }));
    }

    /// The latest run of each workflow for commit `sha`.
    pub fn runs_for(&self, sha: &str) -> Vec<&WorkflowRun> {
        let mut seen = std::collections::HashSet::new();
        self.runs
            .iter()
            .filter(|r| r.head_sha.starts_with(sha) || sha.starts_with(&r.head_sha))
            .filter(|r| seen.insert(r.workflow_name.clone()))
            .collect()
    }

    /// Re-runs a workflow run (only its failed jobs when `failed_only`).
    pub fn rerun(&mut self, run: u64, failed_only: bool) {
        let Some(cli) = self.cli.clone() else { return };
        let task = Pending::spawn(&self.ctx, move || {
            let id = run.to_string();
            let mut args = vec!["run", "rerun", id.as_str()];
            if failed_only {
                args.push("--failed");
            }
            cli.run(&args)
        });
        self.side_tasks.push(("Re-run".into(), task));
    }

    /// Loads a run's (or one job's) log.
    pub fn load_log(
        &self,
        run: u64,
        job: Option<u64>,
        failed_only: bool,
    ) -> Option<Pending<Result<String, String>>> {
        let cli = self.cli.clone()?;
        Some(Pending::spawn(&self.ctx, move || {
            let run = run.to_string();
            let job = job.map(|j| j.to_string());
            let mut args = vec!["run", "view"];
            match &job {
                Some(j) => args.extend(["--job", j.as_str()]),
                None => args.push(run.as_str()),
            }
            args.push(if failed_only { "--log-failed" } else { "--log" });
            let out = cli.run(&args)?;
            if out.trim().is_empty() && failed_only {
                // Nothing failed: show the whole log instead.
                args.pop();
                args.push("--log");
                return cli.run(&args);
            }
            Ok(out)
        }))
    }

    /// `gh` for the repository, for work run outside this type.
    pub fn cli(&self) -> Option<Cli> {
        self.cli.clone()
    }

    /// The open PR whose head is `branch`, if any.
    pub fn pr_for_branch(&self, branch: &str) -> Option<&PrSummary> {
        self.prs
            .iter()
            .find(|p| p.head_ref_name == branch && p.state == "OPEN")
    }

    pub fn create(&mut self, new: NewPr) {
        let Some(cli) = self.cli.clone() else { return };
        let git = Cli::locate("git")
            .map(|g| Cli::new(g, cli.cwd().to_path_buf()).with_env("GIT_TERMINAL_PROMPT", "0"));
        self.create_task = Some(Pending::spawn(&self.ctx, move || {
            if let Some(remote) = &new.push_to {
                let git = git.ok_or("git was not found")?;
                let out = git.output(&["push", "-u", remote.as_str(), new.head.as_str()], None)?;
                if !out.success {
                    return out.into_result();
                }
            }
            let mut args = vec!["pr", "create", "--head", &new.head];
            if !new.base.trim().is_empty() {
                args.extend(["--base", new.base.trim()]);
            }
            if new.draft {
                args.push("--draft");
            }
            if new.title.trim().is_empty() {
                // Title and body from the branch's commits.
                args.push("--fill");
                cli.run(&args)
            } else {
                args.extend(["--title", new.title.trim(), "--body-file", "-"]);
                cli.run_with_input(&args, &new.body)
            }
        }));
    }

    /// Opens the PR detail view model for `number`.
    pub fn open_pr(&self, number: u64) -> Option<PrPane> {
        let cli = self.cli.clone()?;
        Some(PrPane::new(
            self.ctx.clone(),
            cli,
            number,
            self.user.clone(),
        ))
    }
}

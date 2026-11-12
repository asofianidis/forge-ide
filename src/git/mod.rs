//! Git integration: repository status, staging, commits, branches, stashes
//! and history, all driven through the `git` command-line tool on
//! background threads.

pub mod blame;
pub mod diff;
pub mod line_changes;
mod panel;
pub mod status;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::Cli;
use crate::task::{self, Pending};
use diff::FileDiff;
pub use panel::{PanelAction, ScmPanel, change_color as panel_change_color};
use status::{Change, Entry, Status};

/// How often status is re-read while the window has focus.
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);
const LOG_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub email: String,
    /// Unix seconds.
    pub time: i64,
    pub subject: String,
    /// Decorations such as `HEAD -> main, origin/main, tag: v1`.
    pub refs: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// Short name, e.g. `main` or `origin/main`.
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub upstream: Option<String>,
    /// e.g. `[ahead 1, behind 2]`, `[gone]`.
    pub track: String,
    pub time: i64,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// `stash@{N}`.
    pub name: String,
    pub message: String,
    pub time: i64,
}

/// Full details of one commit, for the commit view.
#[derive(Debug, Clone)]
pub struct CommitDetail {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub message: String,
    pub files: Vec<FileDiff>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoState {
    /// No folder is open.
    NoFolder,
    Detecting,
    GitMissing,
    NotARepo,
    Ready,
}

/// Something worth telling the user, reported in the status bar.
#[derive(Debug, Clone)]
pub enum Event {
    Done(String),
    Failed(String),
}

/// Repository root and the workspace's path inside it, or `None` when the
/// folder isn't in a repository.
type DetectResult = Result<Option<(PathBuf, PathBuf)>, String>;

type Work = Box<dyn FnOnce(&Cli) -> Result<String, String> + Send>;

struct Op {
    label: String,
    work: Work,
}

pub struct SourceControl {
    ctx: egui::Context,
    git_path: Option<PathBuf>,
    /// The folder open in the explorer (as given, not canonicalized).
    workspace: Option<PathBuf>,
    /// Path of the workspace folder relative to the repository root.
    workspace_prefix: PathBuf,
    pub state: RepoState,
    detect: Option<Pending<DetectResult>>,
    git: Option<Cli>,

    pub status: Status,
    status_task: Option<Pending<Result<Status, String>>>,
    last_refresh: Option<Instant>,
    /// Bumped whenever status changes or an operation finishes, so views
    /// that show git data know to reload.
    pub revision: u64,
    /// Explorer decorations keyed by repo-relative path (files and dirs).
    decorations: HashMap<String, Change>,

    pub log: Vec<Commit>,
    log_task: Option<Pending<Result<Vec<Commit>, String>>>,
    pub branches: Vec<Branch>,
    branches_task: Option<Pending<Result<Vec<Branch>, String>>>,
    pub stashes: Vec<Stash>,
    stash_task: Option<Pending<Result<Vec<Stash>, String>>>,
    pub remotes: Vec<String>,
    remotes_task: Option<Pending<Result<Vec<String>, String>>>,

    ops: VecDeque<Op>,
    running: Option<(String, Pending<Result<String, String>>)>,
    events: Vec<Event>,
    was_focused: bool,
}

impl SourceControl {
    pub fn new(ctx: egui::Context, workspace: Option<&Path>) -> Self {
        let mut sc = Self {
            ctx,
            git_path: Cli::locate("git"),
            workspace: None,
            workspace_prefix: PathBuf::new(),
            state: RepoState::NoFolder,
            detect: None,
            git: None,
            status: Status::default(),
            status_task: None,
            last_refresh: None,
            revision: 0,
            decorations: HashMap::new(),
            log: Vec::new(),
            log_task: None,
            branches: Vec::new(),
            branches_task: None,
            stashes: Vec::new(),
            stash_task: None,
            remotes: Vec::new(),
            remotes_task: None,
            ops: VecDeque::new(),
            running: None,
            events: Vec::new(),
            was_focused: true,
        };
        sc.open(workspace);
        sc
    }

    /// Switches to the repository containing `folder` (if any).
    pub fn open(&mut self, folder: Option<&Path>) {
        self.workspace = folder.map(Path::to_path_buf);
        self.git = None;
        self.status = Status::default();
        self.decorations.clear();
        self.log.clear();
        self.branches.clear();
        self.stashes.clear();
        self.remotes.clear();
        self.ops.clear();
        self.revision += 1;
        let Some(folder) = folder.map(Path::to_path_buf) else {
            self.state = RepoState::NoFolder;
            return;
        };
        let Some(git_path) = self.git_path.clone() else {
            self.state = RepoState::GitMissing;
            return;
        };
        self.state = RepoState::Detecting;
        self.detect = Some(Pending::spawn(&self.ctx, move || {
            let cli = Cli::new(git_path, folder.clone());
            let out = cli.output(&["rev-parse", "--show-toplevel"], None)?;
            if !out.success {
                return Ok(None);
            }
            let root = PathBuf::from(out.stdout.trim());
            let canonical = folder.canonicalize().unwrap_or(folder);
            let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
            let prefix = canonical
                .strip_prefix(&root_canon)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            Ok(Some((root, prefix)))
        }));
    }

    pub fn is_ready(&self) -> bool {
        self.state == RepoState::Ready
    }

    pub fn request_repaint_after(&self, delay: Duration) {
        self.ctx.request_repaint_after(delay);
    }

    pub fn root(&self) -> Option<&Path> {
        self.git.as_ref().map(Cli::cwd)
    }

    /// Label of the operation in progress, if any.
    pub fn busy(&self) -> Option<&str> {
        self.running.as_ref().map(|(l, _)| l.as_str())
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Re-reads status, history, branches and stashes.
    pub fn refresh(&mut self) {
        self.refresh_status();
        self.refresh_extras();
    }

    fn refresh_status(&mut self) {
        let Some(git) = self.git.clone() else { return };
        if self.status_task.is_some() {
            return;
        }
        self.last_refresh = Some(Instant::now());
        self.status_task = Some(Pending::spawn(&self.ctx, move || {
            let out = git.run(&[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
                "--show-stash",
            ])?;
            Ok(status::parse(&out))
        }));
    }

    fn refresh_extras(&mut self) {
        let Some(git) = self.git.clone() else { return };
        if self.log_task.is_none() {
            let g = git.clone();
            self.log_task = Some(Pending::spawn(&self.ctx, move || load_log(&g, None)));
        }
        if self.branches_task.is_none() {
            let g = git.clone();
            self.branches_task = Some(Pending::spawn(&self.ctx, move || load_branches(&g)));
        }
        if self.stash_task.is_none() {
            let g = git.clone();
            self.stash_task = Some(Pending::spawn(&self.ctx, move || load_stashes(&g)));
        }
        if self.remotes_task.is_none() {
            self.remotes_task = Some(Pending::spawn(&self.ctx, move || {
                Ok(git
                    .run(&["remote"])?
                    .lines()
                    .map(str::to_owned)
                    .filter(|l| !l.is_empty())
                    .collect())
            }));
        }
    }

    /// Picks up finished background work and schedules periodic refreshes.
    /// Call once per frame.
    pub fn poll(&mut self) {
        if let Some(result) = task::poll(&mut self.detect) {
            match result {
                Ok(Some((root, prefix))) => {
                    let git_path = self.git_path.clone().expect("checked in open");
                    self.git = Some(
                        Cli::new(git_path, root)
                            .with_prefix(&["-c", "core.quotePath=false", "-c", "color.ui=false"])
                            .with_env("GIT_TERMINAL_PROMPT", "0")
                            .with_env("GIT_OPTIONAL_LOCKS", "0")
                            .with_env("GIT_EDITOR", "true")
                            .with_env("GIT_MERGE_AUTOEDIT", "no"),
                    );
                    self.workspace_prefix = prefix;
                    self.state = RepoState::Ready;
                    self.refresh();
                }
                Ok(None) => self.state = RepoState::NotARepo,
                Err(e) => {
                    self.state = RepoState::GitMissing;
                    self.events.push(Event::Failed(e));
                }
            }
        }

        if let Some(result) = task::poll(&mut self.status_task) {
            match result {
                Ok(status) => {
                    if status != self.status {
                        self.decorations = decorations(&status);
                        self.status = status;
                        self.revision += 1;
                    }
                }
                Err(e) => {
                    // The repository may have been deleted or moved.
                    if self.root().is_some_and(|r| !r.exists()) {
                        self.state = RepoState::NotARepo;
                        self.git = None;
                    } else {
                        self.events.push(Event::Failed(e));
                    }
                }
            }
        }
        if let Some(Ok(log)) = task::poll(&mut self.log_task) {
            self.log = log;
        }
        if let Some(Ok(branches)) = task::poll(&mut self.branches_task) {
            self.branches = branches;
        }
        if let Some(Ok(stashes)) = task::poll(&mut self.stash_task) {
            self.stashes = stashes;
        }
        if let Some(Ok(remotes)) = task::poll(&mut self.remotes_task) {
            self.remotes = remotes;
        }

        self.poll_ops();

        if self.is_ready() {
            let focused = self.ctx.input(|i| i.focused);
            let regained_focus = focused && !self.was_focused;
            self.was_focused = focused;
            let due = self
                .last_refresh
                .is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL);
            if regained_focus {
                self.refresh();
            } else if focused && due && self.running.is_none() {
                self.refresh_status();
            }
            self.ctx.request_repaint_after(REFRESH_INTERVAL);
        }
    }

    fn poll_ops(&mut self) {
        if let Some((label, pending)) = &self.running
            && let Some(result) = pending.try_take()
        {
            match result {
                Ok(out) => {
                    let msg = out.trim();
                    // Keep the status bar short: first line of output, if any.
                    let detail = msg.lines().last().filter(|l| l.len() < 120);
                    self.events.push(Event::Done(match detail {
                        Some(d) if !d.is_empty() => format!("{label}: {d}"),
                        _ => format!("{label}: done"),
                    }));
                }
                Err(e) => self
                    .events
                    .push(Event::Failed(format!("{label} failed: {e}"))),
            }
            self.running = None;
            self.revision += 1;
            self.refresh();
        }
        if self.running.is_none()
            && let Some(op) = self.ops.pop_front()
            && let Some(git) = self.git.clone()
        {
            let work = op.work;
            self.running = Some((op.label, Pending::spawn(&self.ctx, move || work(&git))));
        }
    }

    /// Queues a mutating operation. Operations run one at a time, in order.
    fn queue(
        &mut self,
        label: impl Into<String>,
        work: impl FnOnce(&Cli) -> Result<String, String> + Send + 'static,
    ) {
        if self.git.is_none() {
            return;
        }
        self.ops.push_back(Op {
            label: label.into(),
            work: Box::new(work),
        });
        self.poll_ops();
    }

    // ---- paths -----------------------------------------------------------

    /// Repository-relative path (with `/` separators) for an absolute path.
    pub fn rel_path(&self, path: &Path) -> Option<String> {
        let rel = if let Some(ws) = &self.workspace
            && let Ok(rest) = path.strip_prefix(ws)
        {
            self.workspace_prefix.join(rest)
        } else {
            let root = self.root()?;
            match path.strip_prefix(root) {
                Ok(rest) => rest.to_path_buf(),
                Err(_) => {
                    let canon = path.canonicalize().ok()?;
                    let root = root.canonicalize().ok()?;
                    canon.strip_prefix(root).ok()?.to_path_buf()
                }
            }
        };
        let parts: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(parts.join("/"))
    }

    pub fn abs_path(&self, rel: &str) -> Option<PathBuf> {
        Some(self.root()?.join(rel))
    }

    /// Git state of a file or folder shown in the explorer.
    pub fn decoration(&self, path: &Path) -> Option<Change> {
        if self.decorations.is_empty() {
            return None;
        }
        self.decorations.get(&self.rel_path(path)?).copied()
    }

    pub fn branch_name(&self) -> Option<String> {
        self.is_ready().then(|| self.status.branch.display())
    }

    // ---- operations ------------------------------------------------------

    pub fn init(&mut self) {
        let Some(folder) = self.workspace.clone() else {
            return;
        };
        let Some(git_path) = self.git_path.clone() else {
            return;
        };
        let ctx = self.ctx.clone();
        // `git init` runs before there is a repository, so not via the queue.
        let result = Cli::new(git_path, folder.clone()).run(&["init"]);
        match result {
            Ok(_) => self
                .events
                .push(Event::Done("Initialized repository".into())),
            Err(e) => self
                .events
                .push(Event::Failed(format!("git init failed: {e}"))),
        }
        ctx.request_repaint();
        self.open(Some(&folder));
    }

    pub fn stage(&mut self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        let label = count_label("Stage", &paths);
        self.queue(label, move |git| {
            let mut args = vec!["add".to_owned(), "-A".to_owned(), "--".to_owned()];
            args.extend(paths);
            git.run(&args)
        });
    }

    pub fn stage_all(&mut self) {
        self.queue("Stage all", |git| git.run(&["add", "-A"]));
    }

    pub fn unstage(&mut self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        let unborn = self.status.branch.is_unborn();
        let label = count_label("Unstage", &paths);
        self.queue(label, move |git| {
            let mut args: Vec<String> = if unborn {
                ["rm", "--cached", "-r", "-q", "--"]
                    .map(String::from)
                    .to_vec()
            } else {
                ["restore", "--staged", "--"].map(String::from).to_vec()
            };
            args.extend(paths);
            git.run(&args)
        });
    }

    pub fn unstage_all(&mut self) {
        let paths = self.status.staged().map(|e| e.path.clone()).collect();
        self.unstage(paths);
    }

    /// Throws away work-tree changes. Untracked files are deleted.
    pub fn discard(&mut self, entries: Vec<Entry>) {
        if entries.is_empty() {
            return;
        }
        let (untracked, tracked): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(Entry::is_untracked);
        let tracked: Vec<String> = tracked.into_iter().map(|e| e.path).collect();
        let untracked: Vec<String> = untracked.into_iter().map(|e| e.path).collect();
        let n = tracked.len() + untracked.len();
        let label = if n == 1 {
            "Discard changes".to_owned()
        } else {
            format!("Discard {n} files")
        };
        self.queue(label, move |git| {
            if !tracked.is_empty() {
                let mut args = vec![
                    "restore".to_owned(),
                    "--worktree".to_owned(),
                    "--".to_owned(),
                ];
                args.extend(tracked);
                git.run(&args)?;
            }
            if !untracked.is_empty() {
                let mut args = vec![
                    "clean".to_owned(),
                    "-f".to_owned(),
                    "-q".to_owned(),
                    "--".to_owned(),
                ];
                args.extend(untracked);
                git.run(&args)?;
            }
            Ok(String::new())
        });
    }

    /// Applies a patch to the index (`cached`) and/or work tree, optionally
    /// in reverse. Used for staging, unstaging and discarding single hunks.
    pub fn apply_patch(&mut self, label: &str, patch: String, cached: bool, reverse: bool) {
        self.queue(label.to_owned(), move |git| {
            let mut args = vec!["apply", "--whitespace=nowarn", "--recount"];
            if cached {
                args.push("--cached");
            }
            if reverse {
                args.push("--reverse");
            }
            args.push("-");
            git.run_with_input(&args, &patch)
        });
    }

    /// Stages one editor change: the lines `start..start + len` (1-based,
    /// `len` 0 for a deletion above `start`) of the saved file `rel`.
    pub fn stage_change(&mut self, rel: String, start: usize, len: usize) {
        self.queue("Stage change", move |git| {
            let out = git.run(&[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--",
                &rel,
            ])?;
            let files = diff::parse(&out);
            let file = files.first().ok_or("no unstaged changes in this file")?;
            for (h, hunk) in file.hunks.iter().enumerate() {
                let picked = change_lines(hunk, start, len);
                if picked.is_empty() {
                    continue;
                }
                let patch = file
                    .lines_patch(h, &picked, false)
                    .ok_or("nothing to stage")?;
                return git.run_with_input(
                    &["apply", "--cached", "--recount", "--whitespace=nowarn", "-"],
                    &patch,
                );
            }
            Err("this change is already staged (or the file has unsaved edits)".into())
        });
    }

    /// Resolves a conflict by taking one side (`--ours`/`--theirs`) and staging.
    pub fn resolve_with(&mut self, path: String, theirs: bool) {
        let side = if theirs { "--theirs" } else { "--ours" };
        self.queue(format!("Resolve {path}"), move |git| {
            git.run(&["checkout", side, "--", &path])?;
            git.run(&["add", "--", &path])
        });
    }

    /// Commits staged changes, or every change when nothing is staged.
    pub fn commit(&mut self, message: String, amend: bool, push_after: bool) {
        let stage_everything = self.status.staged().next().is_none() && !amend;
        let label = if amend { "Amend commit" } else { "Commit" };
        self.queue(label, move |git| {
            if stage_everything {
                git.run(&["add", "-A"])?;
            }
            let mut args = vec!["commit", "--cleanup=strip"];
            if amend {
                args.push("--amend");
            }
            let out = if message.trim().is_empty() && amend {
                args.push("--no-edit");
                git.run(&args)?
            } else {
                args.extend(["-F", "-"]);
                git.run_with_input(&args, &message)?
            };
            Ok(out.lines().next().unwrap_or_default().to_owned())
        });
        if push_after {
            self.push(false);
        }
    }

    /// Undoes the last commit, keeping its changes staged.
    pub fn undo_last_commit(&mut self) {
        self.queue("Undo last commit", |git| {
            git.run(&["reset", "--soft", "HEAD~1"])
        });
    }

    /// `origin` if it exists, otherwise the first remote.
    pub fn default_remote(&self) -> Option<String> {
        if self.remotes.iter().any(|r| r == "origin") {
            Some("origin".to_owned())
        } else {
            self.remotes.first().cloned()
        }
    }

    /// Pushes the current branch, publishing it if it has no upstream.
    pub fn push(&mut self, force: bool) {
        let has_upstream = self.status.branch.upstream.is_some();
        let branch = self.status.branch.head.clone();
        let remote = self.default_remote();
        let label = if has_upstream {
            "Push"
        } else {
            "Publish branch"
        };
        self.queue(label, move |git| {
            let mut args = vec!["push".to_owned()];
            if force {
                args.push("--force-with-lease".to_owned());
            }
            if !has_upstream {
                let remote = remote.ok_or("this repository has no remote to push to")?;
                let branch = branch.ok_or("HEAD is detached; check out a branch first")?;
                args.extend(["-u".to_owned(), remote, branch]);
            }
            // git push reports progress on stderr even on success.
            let out = git.output(&args, None)?;
            if out.success {
                Ok(out.stderr.lines().last().unwrap_or_default().to_owned())
            } else {
                out.into_result()
            }
        });
    }

    pub fn pull(&mut self, rebase: bool) {
        self.queue(if rebase { "Pull (rebase)" } else { "Pull" }, move |git| {
            if rebase {
                git.run(&["pull", "--rebase"])
            } else {
                git.run(&["pull"])
            }
        });
    }

    pub fn fetch(&mut self) {
        self.queue("Fetch", |git| git.run(&["fetch", "--all", "--prune"]));
    }

    /// Pull then push, like the sync button in other editors.
    pub fn sync(&mut self) {
        if self.status.branch.upstream.is_none() {
            self.push(false);
            return;
        }
        self.pull(false);
        self.push(false);
    }

    pub fn checkout(&mut self, branch: &Branch) {
        let name = branch.name.clone();
        let remote = branch.remote;
        // A remote branch with a local counterpart: switch to the local one.
        let local = remote
            .then(|| name.split_once('/').map(|(_, b)| b.to_owned()))
            .flatten()
            .filter(|b| self.branches.iter().any(|x| !x.remote && &x.name == b));
        self.queue(format!("Checkout {name}"), move |git| {
            match (remote, local) {
                (true, Some(local)) => git.run(&["switch", &local]),
                (true, None) => git.run(&["switch", "--track", &name]),
                (false, _) => git.run(&["switch", &name]),
            }
        });
    }

    pub fn create_branch(&mut self, name: String, from: Option<String>) {
        self.queue(format!("Create branch {name}"), move |git| {
            let mut args = vec!["switch".to_owned(), "-c".to_owned(), name];
            args.extend(from);
            git.run(&args)
        });
    }

    pub fn delete_branch(&mut self, name: String, force: bool) {
        self.queue(format!("Delete branch {name}"), move |git| {
            git.run(&["branch", if force { "-D" } else { "-d" }, &name])
        });
    }

    pub fn merge_branch(&mut self, name: String) {
        self.queue(format!("Merge {name}"), move |git| {
            git.run(&["merge", "--no-edit", &name])
        });
    }

    pub fn abort_merge(&mut self) {
        self.queue("Abort merge", |git| git.run(&["merge", "--abort"]));
    }

    pub fn stash(&mut self, message: String, include_untracked: bool) {
        self.queue("Stash", move |git| {
            let mut args = vec!["stash", "push"];
            if include_untracked {
                args.push("--include-untracked");
            }
            if !message.trim().is_empty() {
                args.extend(["-m", message.trim()]);
            }
            git.run(&args)
        });
    }

    pub fn stash_pop(&mut self, name: String) {
        self.queue(format!("Pop {name}"), move |git| {
            git.run(&["stash", "pop", &name])
        });
    }

    pub fn stash_apply(&mut self, name: String) {
        self.queue(format!("Apply {name}"), move |git| {
            git.run(&["stash", "apply", &name])
        });
    }

    pub fn stash_drop(&mut self, name: String) {
        self.queue(format!("Drop {name}"), move |git| {
            git.run(&["stash", "drop", &name])
        });
    }

    // ---- reads (not queued) ----------------------------------------------

    /// Loads the diff of one changed file. `staged` picks index vs work tree.
    pub fn load_file_diff(
        &self,
        entry: &Entry,
        staged: bool,
    ) -> Option<Pending<Result<Vec<FileDiff>, String>>> {
        let git = self.git.clone()?;
        let entry = entry.clone();
        Some(Pending::spawn(&self.ctx, move || {
            let common = [
                "--no-color",
                "--no-ext-diff",
                "--src-prefix=a/",
                "--dst-prefix=b/",
            ];
            let out = if entry.is_untracked() {
                let mut args = vec!["diff", "--no-index"];
                args.extend(common);
                args.extend(["--", "/dev/null", entry.path.as_str()]);
                // --no-index exits with 1 when the files differ.
                let out = git.output(&args, None)?;
                if out.code == Some(1) || out.success {
                    out.stdout
                } else {
                    return out.into_result().map(|_| Vec::new());
                }
            } else {
                let mut args = vec!["diff", "-M"];
                if staged {
                    args.push("--cached");
                }
                args.extend(common);
                args.push("--");
                if let Some(orig) = &entry.orig_path
                    && staged
                {
                    args.push(orig);
                }
                args.push(&entry.path);
                git.run(&args)?
            };
            Ok(diff::parse(&out))
        }))
    }

    pub fn load_commit(&self, hash: &str) -> Option<Pending<Result<CommitDetail, String>>> {
        let git = self.git.clone()?;
        let hash = hash.to_owned();
        Some(Pending::spawn(&self.ctx, move || {
            let out = git.run(&[
                "show",
                "--format=%H%x1f%an%x1f%ae%x1f%at%x1f%B%x1e",
                "--patch",
                "-M",
                "--diff-merges=first-parent",
                "--no-color",
                "--no-ext-diff",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                &hash,
            ])?;
            let (meta, patch) = out.split_once('\x1e').unwrap_or((&out, ""));
            let f: Vec<&str> = meta.splitn(5, '\x1f').collect();
            let field = |i: usize| f.get(i).map(|s| s.trim().to_owned()).unwrap_or_default();
            Ok(CommitDetail {
                hash: field(0),
                author: field(1),
                email: field(2),
                time: field(3).parse().unwrap_or(0),
                message: field(4),
                files: diff::parse(patch),
            })
        }))
    }

    /// Who last changed each line of `contents` (the editor buffer for the
    /// file at `rel`), so unsaved edits line up with what's on screen.
    pub fn load_blame(
        &self,
        rel: &str,
        contents: String,
    ) -> Option<Pending<Result<blame::Blame, String>>> {
        let git = self.git.clone()?;
        let rel = rel.to_owned();
        Some(Pending::spawn(&self.ctx, move || {
            let out = git.run_with_input(
                &["blame", "--porcelain", "--contents", "-", "--", &rel],
                &contents,
            )?;
            Ok(blame::parse(&out))
        }))
    }

    /// The committed (HEAD) content of a file, `None` if it is not in HEAD.
    pub fn load_head_content(&self, rel: &str) -> Option<Pending<Option<String>>> {
        let git = self.git.clone()?;
        let spec = format!("HEAD:{rel}");
        Some(Pending::spawn(&self.ctx, move || {
            let out = git.output(&["show", &spec], None).ok()?;
            out.success.then_some(out.stdout)
        }))
    }
}

/// Indices of the lines in `hunk` that make up the editor change at
/// `start..start + len`: added lines in that range, plus the removed lines
/// directly before it.
fn change_lines(hunk: &diff::Hunk, start: usize, len: usize) -> Vec<usize> {
    use diff::LineKind;
    let start = start as u32;
    let end = start + len as u32;
    let mut picked = Vec::new();
    for (i, line) in hunk.lines.iter().enumerate() {
        match line.kind {
            LineKind::Added => {
                if line.new_no.is_some_and(|n| n >= start && n < end) {
                    picked.push(i);
                }
            }
            LineKind::Removed => {
                // The new-file line this removal sits above.
                let above = hunk.lines[i..]
                    .iter()
                    .find_map(|l| l.new_no)
                    .unwrap_or(hunk.new_start + hunk.new_count);
                if above == start {
                    picked.push(i);
                }
            }
            LineKind::Context => {}
        }
    }
    picked
}

fn count_label(verb: &str, paths: &[String]) -> String {
    match paths {
        [one] => format!("{verb} {}", one.rsplit('/').next().unwrap_or(one)),
        _ => format!("{verb} {} files", paths.len()),
    }
}

/// Builds the explorer decoration map: every changed file, plus each parent
/// folder (which takes its most important child change).
fn decorations(status: &Status) -> HashMap<String, Change> {
    fn rank(c: Change) -> u8 {
        match c {
            Change::Conflicted => 4,
            Change::Modified | Change::Renamed | Change::TypeChanged | Change::Copied => 3,
            Change::Deleted => 2,
            Change::Added | Change::Untracked => 1,
        }
    }
    let mut map = HashMap::new();
    for entry in &status.entries {
        let change = entry.primary();
        map.insert(entry.path.clone(), change);
        let mut dir = entry.path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            let slot = map.entry(parent.to_owned()).or_insert(change);
            if rank(change) > rank(*slot) {
                *slot = change;
            }
            dir = parent;
        }
        // The repository root itself.
        let slot = map.entry(String::new()).or_insert(change);
        if rank(change) > rank(*slot) {
            *slot = change;
        }
    }
    map
}

fn load_log(git: &Cli, path: Option<&str>) -> Result<Vec<Commit>, String> {
    let limit = format!("-n{LOG_LIMIT}");
    let mut args = vec![
        "log",
        &limit,
        "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%at%x1f%s%x1f%D%x1e",
    ];
    if let Some(p) = path {
        args.extend(["--follow", "--", p]);
    }
    let out = git.output(&args, None)?;
    if !out.success {
        // A branch with no commits yet has no history.
        return Ok(Vec::new());
    }
    Ok(parse_log(&out.stdout))
}

fn parse_log(out: &str) -> Vec<Commit> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            if rec.is_empty() {
                return None;
            }
            let f: Vec<&str> = rec.split('\x1f').collect();
            Some(Commit {
                hash: f.first()?.to_string(),
                short: f.get(1)?.to_string(),
                author: f.get(2)?.to_string(),
                email: f.get(3)?.to_string(),
                time: f.get(4)?.parse().unwrap_or(0),
                subject: f.get(5)?.to_string(),
                refs: f.get(6).map(|s| s.trim().to_owned()).unwrap_or_default(),
            })
        })
        .collect()
}

fn load_branches(git: &Cli) -> Result<Vec<Branch>, String> {
    let out = git.run(&[
        "for-each-ref",
        "--sort=-committerdate",
        "--format=%(refname)%1f%(refname:short)%1f%(HEAD)%1f%(upstream:short)%1f%(upstream:track)%1f%(committerdate:unix)%1f%(contents:subject)%1e",
        "refs/heads",
        "refs/remotes",
    ])?;
    Ok(parse_branches(&out))
}

fn parse_branches(out: &str) -> Vec<Branch> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            let f: Vec<&str> = rec.split('\x1f').collect();
            let refname = *f.first()?;
            // Skip symbolic refs like origin/HEAD.
            if refname.ends_with("/HEAD") || refname.is_empty() {
                return None;
            }
            Some(Branch {
                name: f.get(1)?.to_string(),
                remote: refname.starts_with("refs/remotes/"),
                current: f.get(2)? == &"*",
                upstream: f.get(3).filter(|s| !s.is_empty()).map(|s| s.to_string()),
                track: f.get(4).map(|s| s.to_string()).unwrap_or_default(),
                time: f.get(5).and_then(|s| s.parse().ok()).unwrap_or(0),
                subject: f.get(6).map(|s| s.to_string()).unwrap_or_default(),
            })
        })
        .collect()
}

fn load_stashes(git: &Cli) -> Result<Vec<Stash>, String> {
    let out = git.run(&["stash", "list", "--format=%gd%x1f%s%x1f%ct"])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\x1f').collect();
            Some(Stash {
                name: f.first()?.to_string(),
                message: f.get(1)?.to_string(),
                time: f.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_records() {
        let out = "abc\x1fa\x1fAda\x1fada@x\x1f100\x1fFirst\x1fHEAD -> main\x1e\n\
                   def\x1fd\x1fBob\x1fbob@x\x1f50\x1fSecond: with \x1f\x1e\n";
        let log = parse_log(out);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].refs, "HEAD -> main");
        assert_eq!(log[1].subject, "Second: with ");
        assert_eq!(log[1].time, 50);
    }

    #[test]
    fn parses_branches_and_skips_symbolic_refs() {
        let out = "refs/heads/main\x1fmain\x1f*\x1forigin/main\x1f[ahead 1]\x1f10\x1fmsg\x1e\n\
                   refs/remotes/origin/HEAD\x1forigin\x1f \x1f\x1f\x1f10\x1fmsg\x1e\n\
                   refs/remotes/origin/dev\x1forigin/dev\x1f \x1f\x1f\x1f5\x1fother\x1e\n";
        let b = parse_branches(out);
        assert_eq!(b.len(), 2);
        assert!(b[0].current && !b[0].remote);
        assert_eq!(b[0].upstream.as_deref(), Some("origin/main"));
        assert!(b[1].remote && !b[1].current);
    }

    #[test]
    fn change_lines_pick_one_editor_change() {
        let d = diff::parse(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n a\n-b\n+B\n c\n-d\n e\n+new\n f\n",
        );
        let hunk = &d[0].hunks[0];
        // Modified line 2: the removal of "b" and the addition of "B".
        assert_eq!(change_lines(hunk, 2, 1), [1, 2]);
        // "d" was deleted above new line 4 ("e").
        assert_eq!(change_lines(hunk, 4, 0), [4]);
        // Added line 5.
        assert_eq!(change_lines(hunk, 5, 1), [6]);
        assert!(change_lines(hunk, 1, 1).is_empty());
    }

    #[test]
    fn decorations_propagate_to_parent_dirs() {
        let s = status::parse("1 .M N... 100644 100644 100644 a b src/deep/a.rs\0? src/new.rs\0");
        let d = decorations(&s);
        assert_eq!(d.get("src/deep/a.rs"), Some(&Change::Modified));
        assert_eq!(d.get("src/deep"), Some(&Change::Modified));
        assert_eq!(d.get("src"), Some(&Change::Modified));
        assert_eq!(d.get("src/new.rs"), Some(&Change::Untracked));
    }

    /// Exercises real git end to end: stage, commit, hunk staging, status.
    #[test]
    fn end_to_end_with_real_git() {
        let Some(git_path) = Cli::locate("git") else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("forge-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = Cli::new(git_path, dir.clone()).with_prefix(&[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ]);
        git.run(&["init", "-q"]).unwrap();
        std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
        git.run(&["add", "-A"]).unwrap();
        git.run(&["commit", "-q", "-m", "init"]).unwrap();

        // Two separate hunks; stage only the second.
        std::fs::write(dir.join("a.txt"), "one\n2\n3\n4\n5\n6\n7\n8\n9\nten\n").unwrap();
        let out = git
            .run(&[
                "diff",
                "--no-color",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "-U1",
            ])
            .unwrap();
        let files = diff::parse(&out);
        assert_eq!(files[0].hunks.len(), 2);
        let patch = files[0].hunk_patch(1).unwrap();
        git.run_with_input(&["apply", "--cached", "-"], &patch)
            .unwrap();

        let status = status::parse(
            &git.run(&["status", "--porcelain=v2", "--branch", "-z"])
                .unwrap(),
        );
        let entry = status.find("a.txt").unwrap();
        assert_eq!(entry.staged, Some(Change::Modified));
        assert_eq!(entry.unstaged, Some(Change::Modified));
        let cached = git.run(&["diff", "--cached"]).unwrap();
        assert!(cached.contains("+ten") && !cached.contains("+one"));

        // Line staging: of the two lines added below, stage only the first.
        git.run(&["add", "-A"]).unwrap();
        git.run(&["commit", "-q", "-m", "second"]).unwrap();
        std::fs::write(
            dir.join("a.txt"),
            "one\n2\nX\nY\n3\n4\n5\n6\n7\n8\n9\nten\n",
        )
        .unwrap();
        let out = git
            .run(&["diff", "--no-color", "--src-prefix=a/", "--dst-prefix=b/"])
            .unwrap();
        let files = diff::parse(&out);
        let x = files[0].hunks[0]
            .lines
            .iter()
            .position(|l| l.text == "X")
            .unwrap();
        let patch = files[0].lines_patch(0, &[x], false).unwrap();
        git.run_with_input(&["apply", "--cached", "--recount", "-"], &patch)
            .unwrap();
        let cached = git.run(&["diff", "--cached"]).unwrap();
        assert!(cached.contains("+X") && !cached.contains("+Y"), "{cached}");

        // Unstage it again by selecting the line in the staged diff.
        let out = git
            .run(&[
                "diff",
                "--cached",
                "--no-color",
                "--src-prefix=a/",
                "--dst-prefix=b/",
            ])
            .unwrap();
        let files = diff::parse(&out);
        let x = files[0].hunks[0]
            .lines
            .iter()
            .position(|l| l.text == "X")
            .unwrap();
        let patch = files[0].lines_patch(0, &[x], true).unwrap();
        git.run_with_input(
            &["apply", "--cached", "--recount", "--reverse", "-"],
            &patch,
        )
        .unwrap();
        assert!(git.run(&["diff", "--cached"]).unwrap().is_empty());

        // Discard only Y from the work tree.
        let out = git
            .run(&["diff", "--no-color", "--src-prefix=a/", "--dst-prefix=b/"])
            .unwrap();
        let files = diff::parse(&out);
        let y = files[0].hunks[0]
            .lines
            .iter()
            .position(|l| l.text == "Y")
            .unwrap();
        let patch = files[0].lines_patch(0, &[y], true).unwrap();
        git.run_with_input(&["apply", "--recount", "--reverse", "-"], &patch)
            .unwrap();
        let content = std::fs::read_to_string(dir.join("a.txt")).unwrap();
        assert!(
            content.contains("X\n") && !content.contains("Y\n"),
            "{content}"
        );

        let log = load_log(&git, None).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[1].subject, "init");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

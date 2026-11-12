//! Language servers: a catalog of known servers, detection of which are
//! installed, one-click installation through the usual toolchains, and an LSP
//! client that runs them for the open files.
//!
//! - [`ServerManager`] finds and installs servers.
//! - [`Lsp`] (in `session`) starts installed servers, keeps them in sync with
//!   the buffers and turns their replies into [`LspEvent`]s.
//! - `client` runs one server process; `rpc` frames JSON-RPC over stdio;
//!   `text` converts positions, URIs, snippets and edits.

mod browser;
pub mod catalog;
mod client;
pub mod detect;
mod install;
mod rpc;
mod session;
pub mod text;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

pub use browser::ServerBrowser;
use catalog::{InstallMethod, ServerSpec};
use install::{InstallJob, JobState};
pub use session::{
    Feature, Location, LocationKind, Lsp, LspEvent, Problem, ServerActivity, Severity,
    workspace_edit_files,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Checking,
    Installed(PathBuf),
    Missing,
}

/// Result of one background detection pass.
struct Detection {
    servers: Vec<(&'static str, Option<PathBuf>)>,
    tools: HashMap<&'static str, Option<PathBuf>>,
}

/// Tracks which servers are installed and runs install jobs.
pub struct ServerManager {
    ctx: egui::Context,
    status: HashMap<&'static str, ServerStatus>,
    /// Install programs (npm, rustup, …) and where they were found.
    tools: HashMap<&'static str, Option<PathBuf>>,
    detection: Option<Receiver<Detection>>,
    jobs: HashMap<&'static str, InstallJob>,
    /// Jobs that were running last frame; when one finishes we re-detect.
    running: HashSet<&'static str>,
}

impl ServerManager {
    pub fn new(ctx: egui::Context) -> Self {
        let mut manager = Self {
            ctx,
            status: HashMap::new(),
            tools: HashMap::new(),
            detection: None,
            jobs: HashMap::new(),
            running: HashSet::new(),
        };
        manager.refresh();
        manager
    }

    /// Re-scans the system for installed servers and package managers.
    pub fn refresh(&mut self) {
        for spec in catalog::servers() {
            self.status.entry(spec.id).or_insert(ServerStatus::Checking);
        }
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let servers = catalog::servers()
                .iter()
                .map(|s| (s.id, detect::detect_server(s)))
                .collect();
            let tools = catalog::servers()
                .iter()
                .flat_map(|s| s.install_methods())
                .map(|m| m.program)
                .collect::<HashSet<_>>()
                .into_iter()
                .map(|p| (p, detect::find_executable(p)))
                .collect();
            let _ = tx.send(Detection { servers, tools });
            ctx.request_repaint();
        });
        self.detection = Some(rx);
    }

    /// Picks up finished background work. Call once per frame.
    pub fn poll(&mut self) {
        if let Some(rx) = &self.detection
            && let Ok(d) = rx.try_recv()
        {
            for (id, path) in d.servers {
                let status = path.map_or(ServerStatus::Missing, ServerStatus::Installed);
                self.status.insert(id, status);
            }
            self.tools = d.tools;
            self.detection = None;
        }

        let finished: Vec<_> = self
            .running
            .iter()
            .copied()
            .filter(|id| self.job_state(id) != Some(JobState::Running))
            .collect();
        if !finished.is_empty() {
            self.running.retain(|id| !finished.contains(id));
            self.refresh();
        }
    }

    pub fn is_detecting(&self) -> bool {
        self.detection.is_some()
    }

    pub fn status(&self, id: &str) -> ServerStatus {
        self.status
            .get(id)
            .cloned()
            .unwrap_or(ServerStatus::Checking)
    }

    pub fn is_installed(&self, id: &str) -> bool {
        matches!(self.status(id), ServerStatus::Installed(_))
    }

    /// Where an install program was found, or `None` if it is missing or
    /// detection has not finished yet.
    pub fn tool_path(&self, program: &str) -> Option<&PathBuf> {
        self.tools.get(program).and_then(Option::as_ref)
    }

    /// The best install method for this OS: the first whose program is
    /// available, otherwise the first applicable one.
    pub fn preferred_method(&self, spec: &'static ServerSpec) -> Option<&'static InstallMethod> {
        spec.install_methods()
            .find(|m| self.tool_path(m.program).is_some())
            .or_else(|| spec.install_methods().next())
    }

    pub fn install(&mut self, spec: &'static ServerSpec, method: &'static InstallMethod) {
        let Some(program) = self.tool_path(method.program).cloned() else {
            return;
        };
        let job = InstallJob::spawn(method, &program, self.ctx.clone());
        self.jobs.insert(spec.id, job);
        self.running.insert(spec.id);
    }

    pub fn job(&self, id: &str) -> Option<&InstallJob> {
        self.jobs.get(id)
    }

    pub fn job_state(&self, id: &str) -> Option<JobState> {
        self.jobs.get(id).map(InstallJob::state)
    }

    pub fn dismiss_job(&mut self, id: &str) {
        if self.job_state(id) != Some(JobState::Running) {
            self.jobs.remove(id);
        }
    }

    /// The first installed server for `file_name` and its executable.
    pub fn installed_server_for(&self, file_name: &str) -> Option<(&'static ServerSpec, PathBuf)> {
        catalog::servers_for_file(file_name).find_map(|s| match self.status.get(s.id) {
            Some(ServerStatus::Installed(path)) => Some((s, path.clone())),
            _ => None,
        })
    }

    /// Servers for `file_name` when none of them is installed yet — i.e. the
    /// user would benefit from installing one. Empty while still detecting.
    pub fn suggestions_for_file(&self, file_name: &str) -> Vec<&'static ServerSpec> {
        let candidates: Vec<_> = catalog::servers_for_file(file_name).collect();
        let settled = candidates
            .iter()
            .all(|s| self.status(s.id) != ServerStatus::Checking);
        if !settled || candidates.iter().any(|s| self.is_installed(s.id)) {
            return Vec::new();
        }
        candidates
    }
}

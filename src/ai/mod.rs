//! "BYO AI": drive the agent CLIs already installed on this machine (Claude
//! Code, Cursor and Codex) from the IDE. Threads are conversations kept per
//! folder; a separate, usually cheaper, model writes commit messages and pull
//! request text.

mod flow;
mod panel;
pub mod picker;
pub mod provider;
pub mod textgen;
pub mod thread;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use flow::{CommitJob, FlowDone, FlowKind, FlowPlan, GitFlow, PrJob};
pub use panel::{GitSummary, SidebarAction, ThreadAction};
pub use provider::{Access, Provider};
pub use textgen::TextGenerator;
pub use thread::ModelChoice;

use picker::PickerState;
use thread::{Message, Run, RunEvent, Thread};

use crate::settings::AiSettings;

/// A turn in progress and the reply streamed so far.
struct ActiveRun {
    run: Run,
    streaming: String,
    stopped: bool,
}

pub struct Ai {
    ctx: egui::Context,
    /// Where each CLI is installed, if it is.
    installed: Vec<(Provider, Option<PathBuf>)>,
    /// Threads for every folder; the view shows those for `folder`.
    threads: Vec<Thread>,
    runs: HashMap<String, ActiveRun>,
    folder: Option<PathBuf>,
    /// Threads changed since they were last saved.
    dirty: bool,
    agent_picker: PickerState,
    text_picker: PickerState,
    thread_picker: PickerState,
    /// Unsent text per thread.
    drafts: HashMap<String, String>,
    /// Thread being renamed and its new name.
    renaming: Option<(String, String)>,
    pub flow: Option<GitFlow>,
    /// Last finished git flow: (succeeded, message, PR URL).
    flow_result: Option<(bool, String, Option<String>)>,
    status: Vec<String>,
    /// Titles being written by the text model: (thread id, result).
    title_tasks: Vec<(String, crate::task::Pending<Result<String, String>>)>,
}

impl Ai {
    pub fn new(ctx: egui::Context, folder: Option<&Path>) -> Self {
        Self {
            ctx,
            installed: detect(),
            threads: thread::load_all(),
            runs: HashMap::new(),
            folder: folder.map(Path::to_path_buf),
            dirty: false,
            agent_picker: PickerState::default(),
            text_picker: PickerState::default(),
            thread_picker: PickerState::default(),
            drafts: HashMap::new(),
            renaming: None,
            flow: None,
            flow_result: None,
            status: Vec::new(),
            title_tasks: Vec::new(),
        }
    }

    pub fn set_folder(&mut self, folder: Option<&Path>) {
        self.folder = folder.map(Path::to_path_buf);
    }

    /// Looks for the CLIs again (e.g. after installing one).
    pub fn redetect(&mut self) {
        self.installed = detect();
    }

    pub fn exe(&self, p: Provider) -> Option<&Path> {
        self.installed
            .iter()
            .find(|(x, _)| *x == p)
            .and_then(|(_, path)| path.as_deref())
    }

    pub fn is_installed(&self, p: Provider) -> bool {
        self.exe(p).is_some()
    }

    fn first_installed(&self) -> Option<Provider> {
        Provider::ALL.into_iter().find(|p| self.is_installed(*p))
    }

    /// The agent model for new threads.
    pub fn agent_choice(&self, settings: &AiSettings) -> ModelChoice {
        settings.agent.clone().unwrap_or_else(|| {
            match self.first_installed().unwrap_or(Provider::Claude) {
                Provider::Claude => ModelChoice::new(Provider::Claude, "claude-opus-5-5"),
                Provider::Cursor => ModelChoice::new(Provider::Cursor, "auto"),
                Provider::Codex => ModelChoice::new(Provider::Codex, ""),
            }
        })
    }

    /// The model that writes commit messages and PR text.
    pub fn text_choice(&self, settings: &AiSettings) -> ModelChoice {
        settings.text.clone().unwrap_or_else(|| {
            match self.first_installed().unwrap_or(Provider::Claude) {
                Provider::Claude => ModelChoice {
                    provider: Provider::Claude,
                    model: "claude-haiku-5-5".to_owned(),
                    effort: "low".to_owned(),
                },
                Provider::Cursor => ModelChoice::new(Provider::Cursor, "auto"),
                Provider::Codex => ModelChoice {
                    provider: Provider::Codex,
                    model: String::new(),
                    effort: "low".to_owned(),
                },
            }
        })
    }

    /// The text model, if its CLI is installed.
    pub fn text_generator(&self, settings: &AiSettings) -> Option<TextGenerator> {
        let choice = self.text_choice(settings);
        let exe = self.exe(choice.provider)?.to_path_buf();
        Some(TextGenerator { choice, exe })
    }

    // ---- threads -----------------------------------------------------------

    /// Threads for the open folder, most recently active first.
    fn folder_threads(&self) -> Vec<&Thread> {
        let Some(folder) = &self.folder else {
            return Vec::new();
        };
        let mut list: Vec<&Thread> = self
            .threads
            .iter()
            .filter(|t| &t.folder == folder)
            .collect();
        list.sort_by_key(|t| std::cmp::Reverse(t.updated));
        list
    }

    pub fn thread(&self, id: &str) -> Option<&Thread> {
        self.threads.iter().find(|t| t.id == id)
    }

    fn thread_mut(&mut self, id: &str) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == id)
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.runs.contains_key(id)
    }

    pub fn running_count(&self) -> usize {
        self.runs.len()
    }

    /// Starts an empty thread in the open folder and returns its id.
    pub fn new_thread(&mut self, settings: &AiSettings) -> Option<String> {
        let folder = self.folder.clone()?;
        let t = Thread::new(folder, self.agent_choice(settings), settings.access);
        let id = t.id.clone();
        self.threads.push(t);
        self.dirty = true;
        self.save();
        Some(id)
    }

    pub fn delete_thread(&mut self, id: &str) {
        if let Some(run) = self.runs.remove(id) {
            run.run.stop();
        }
        self.threads.retain(|t| t.id != id);
        self.drafts.remove(id);
        self.dirty = true;
        self.save();
    }

    /// Sends `text` to the thread's agent. On a thread's first message,
    /// `titler` (the text model) names the thread in the background.
    pub fn send(
        &mut self,
        id: &str,
        text: String,
        titler: Option<TextGenerator>,
    ) -> Result<(), String> {
        if self.runs.contains_key(id) {
            return Err("the agent is still working on the last message".to_owned());
        }
        let ctx = self.ctx.clone();
        let (provider, exe) = {
            let t = self.thread(id).ok_or("thread not found")?;
            let p = t.choice.provider;
            let exe = self
                .exe(p)
                .ok_or_else(|| format!("the {} CLI isn't installed", p.name()))?
                .to_path_buf();
            (p, exe)
        };
        let t = self.thread_mut(id).ok_or("thread not found")?;
        if !t.folder.is_dir() {
            return Err(format!("{} no longer exists", t.folder.display()));
        }
        let (args, stdin) = provider.agent_args(
            &t.folder,
            &t.choice.model,
            &t.choice.effort,
            t.access,
            t.session.as_deref(),
            &text,
        );
        let run = Run::start(&ctx, &exe, provider, &t.folder, args, stdin)?;
        let first = !t.messages.iter().any(|m| matches!(m, Message::User { .. }));
        if first
            && !t.title_locked
            && let Some(r#gen) = titler
        {
            let folder = t.folder.clone();
            let prompt = text.clone();
            let task =
                crate::task::Pending::spawn(&ctx, move || r#gen.thread_title(&folder, &prompt));
            self.title_tasks.push((id.to_owned(), task));
        }
        let t = self.thread_mut(id).ok_or("thread not found")?;
        // A placeholder until the generated title arrives.
        t.set_title_from(&text);
        t.messages.push(Message::User { text });
        t.updated = thread::now();
        self.runs.insert(
            id.to_owned(),
            ActiveRun {
                run,
                streaming: String::new(),
                stopped: false,
            },
        );
        self.dirty = true;
        self.save();
        Ok(())
    }

    pub fn stop(&mut self, id: &str) {
        if let Some(r) = self.runs.get_mut(id) {
            r.stopped = true;
            r.run.stop();
        }
    }

    /// Reads output from running agents. Returns true when a turn ended
    /// (files may have changed).
    pub fn poll(&mut self) -> bool {
        let mut finished = Vec::new();
        for (id, active) in &mut self.runs {
            let Some(t) = self.threads.iter_mut().find(|t| &t.id == id) else {
                finished.push(id.clone());
                continue;
            };
            while let Ok(ev) = active.run.rx.try_recv() {
                match ev {
                    RunEvent::Agent(ev) => t.apply(ev, &mut active.streaming),
                    RunEvent::Exited { success, stderr } => {
                        t.flush(&mut active.streaming);
                        let ended_with_error =
                            matches!(t.messages.last(), Some(Message::Error { .. }));
                        if active.stopped {
                            t.messages.push(Message::Info {
                                text: "Stopped".to_owned(),
                            });
                        } else if !success && !ended_with_error {
                            let text = if stderr.trim().is_empty() {
                                format!("{} exited with an error", t.choice.provider.name())
                            } else {
                                stderr
                            };
                            t.messages.push(Message::Error { text });
                        }
                        t.updated = thread::now();
                        finished.push(id.clone());
                        break;
                    }
                }
            }
        }
        // Keep the elapsed-time counter ticking.
        if !self.runs.is_empty() {
            self.ctx
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
        // Generated titles. Failures keep the placeholder.
        let mut titled = false;
        self.title_tasks.retain(|(id, task)| {
            let Some(result) = task.try_take() else {
                return true;
            };
            if let Ok(title) = result
                && let Some(t) = self.threads.iter_mut().find(|t| &t.id == id)
                && !t.title_locked
            {
                t.title = title;
                titled = true;
            }
            false
        });
        if titled {
            self.dirty = true;
        }
        let any = !finished.is_empty();
        for id in finished {
            self.runs.remove(&id);
            self.dirty = true;
        }
        if any || titled {
            self.save();
        }
        any
    }

    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        match thread::save_all(&self.threads) {
            Ok(()) => self.dirty = false,
            Err(e) => self.status.push(format!("Could not save AI threads: {e}")),
        }
    }

    /// Messages for the status bar.
    pub fn take_status(&mut self) -> Vec<String> {
        std::mem::take(&mut self.status)
    }

    /// Stops every agent (the window is closing).
    pub fn shutdown(&mut self) {
        for r in self.runs.values() {
            r.run.stop();
        }
        self.save();
    }

    // ---- git ---------------------------------------------------------------

    pub fn start_flow(&mut self, plan: FlowPlan) {
        self.flow_result = None;
        self.flow = Some(GitFlow::start(&self.ctx, plan));
    }

    /// Checks the running git flow. Returns its result when it finishes.
    pub fn poll_flow(&mut self) -> Option<Result<FlowDone, String>> {
        let result = self.flow.as_mut()?.poll()?;
        self.flow = None;
        self.flow_result = Some(match &result {
            Ok(done) => (true, done.summary.clone(), done.pr_url.clone()),
            Err(e) => (false, e.clone(), None),
        });
        Some(result)
    }
}

fn detect() -> Vec<(Provider, Option<PathBuf>)> {
    Provider::ALL.map(|p| (p, p.locate())).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Runs the real Claude CLI: two turns in one thread (the second must
    /// resume the first) and a commit message from the text model.
    /// `cargo test -- --ignored claude_cli` with `claude` installed and signed in.
    #[test]
    #[ignore = "needs the claude CLI and makes paid model calls"]
    fn claude_cli_end_to_end() {
        let dir = std::env::temp_dir().join(format!("forge-ai-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Keep the user's thread store out of it.
        unsafe { std::env::set_var("FORGE_AI_THREADS", dir.join("threads.json")) };
        let mut ai = Ai::new(egui::Context::default(), Some(&dir));
        assert!(ai.is_installed(Provider::Claude), "claude CLI not found");
        let settings = AiSettings {
            agent: Some(ModelChoice {
                provider: Provider::Claude,
                model: "claude-haiku-5-5".into(),
                effort: "low".into(),
            }),
            ..Default::default()
        };
        let id = ai.new_thread(&settings).unwrap();

        let wait = |ai: &mut Ai| {
            let start = Instant::now();
            while ai.is_running(&id) {
                ai.poll();
                assert!(start.elapsed() < Duration::from_secs(180), "timed out");
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        let titler = ai.text_generator(&AiSettings::default());
        ai.send(
            &id,
            "Create a file named hello.txt containing exactly the word forge. Then reply DONE."
                .into(),
            titler,
        )
        .unwrap();
        wait(&mut ai);
        let t = ai.thread(&id).unwrap();
        eprintln!("{:#?}", t.messages);
        assert!(t.session.is_some(), "no session id");
        assert!(t.messages.iter().any(|m| matches!(m, Message::Tool { .. })));
        assert_eq!(
            std::fs::read_to_string(dir.join("hello.txt"))
                .unwrap()
                .trim(),
            "forge"
        );

        ai.send(
            &id,
            "What was the name of the file you created? Reply with just the name.".into(),
            None,
        )
        .unwrap();
        wait(&mut ai);
        let t = ai.thread(&id).unwrap();
        let last = t
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Assistant { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(last.contains("hello.txt"), "didn't resume: {last}");
        // The generated title replaces the first-line placeholder.
        let start = Instant::now();
        while !ai.title_tasks.is_empty() && start.elapsed() < Duration::from_secs(60) {
            ai.poll();
            std::thread::sleep(Duration::from_millis(100));
        }
        let title = ai.thread(&id).unwrap().title.clone();
        eprintln!("thread title: {title}");
        assert!(
            !title.starts_with("Create a file named"),
            "title not generated: {title}"
        );

        let r#gen = ai.text_generator(&AiSettings::default()).unwrap();
        let msg = r#gen
            .commit_message(
                &dir,
                "diff --git a/hello.txt b/hello.txt\nnew file mode 100644\n--- /dev/null\n+++ b/hello.txt\n@@ -0,0 +1 @@\n+forge\n",
                &[],
            )
            .unwrap();
        eprintln!("commit message: {msg}");
        assert!(!msg.is_empty() && !msg.contains("```"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

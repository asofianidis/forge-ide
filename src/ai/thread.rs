//! Conversations with an agent, saved per folder, and the process that runs
//! one turn of a conversation.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::provider::{Access, AgentEvent, Provider, augmented_path};
use crate::lsp::detect::hide_console_window;

/// Which CLI, model and settings to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub provider: Provider,
    /// `--model` value; empty for the CLI's default.
    #[serde(default)]
    pub model: String,
    /// Reasoning level; empty for none / the CLI's default.
    #[serde(default)]
    pub effort: String,
}

impl ModelChoice {
    pub fn new(provider: Provider, model: &str) -> Self {
        Self {
            provider,
            model: model.to_owned(),
            effort: provider.default_effort_for(model),
        }
    }

    /// "Opus 5.5 Medium" style label for the picker button.
    pub fn label(&self) -> String {
        let name = self.provider.model_name(&self.model);
        let name = name.strip_prefix("Claude ").unwrap_or(&name).to_owned();
        match self.provider.effort_label(&self.model, &self.effort) {
            Some(e) => format!("{name} {e}"),
            None => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Message {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Tool {
        id: String,
        name: String,
        detail: String,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        is_error: bool,
    },
    Error {
        text: String,
    },
    /// A note from Forge, e.g. "Stopped" or the turn's cost.
    Info {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub title: String,
    /// The folder the agent works in.
    pub folder: PathBuf,
    pub choice: ModelChoice,
    #[serde(default)]
    pub access: Access,
    /// The CLI's conversation id, so later turns resume it.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Seconds since the Unix epoch.
    pub created: u64,
    pub updated: u64,
    /// Renamed by hand, so a generated title must not replace it.
    #[serde(default)]
    pub title_locked: bool,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Thread {
    pub fn new(folder: PathBuf, choice: ModelChoice, access: Access) -> Self {
        let t = now();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        Self {
            id: format!("{t:x}{nanos:08x}"),
            title: "New thread".to_owned(),
            folder,
            choice,
            access,
            session: None,
            messages: Vec::new(),
            created: t,
            updated: t,
            title_locked: false,
        }
    }

    /// Applies an event from the running turn. Streamed text collects in
    /// `streaming` until a complete message replaces it.
    pub fn apply(&mut self, event: AgentEvent, streaming: &mut String) {
        self.updated = now();
        match event {
            AgentEvent::Session(id) => self.session = Some(id),
            AgentEvent::TextDelta(t) => streaming.push_str(&t),
            AgentEvent::Text(t) => {
                streaming.clear();
                self.push_text(t);
            }
            AgentEvent::Tool { id, name, detail } => {
                self.flush(streaming);
                self.messages.push(Message::Tool {
                    id,
                    name,
                    detail,
                    output: None,
                    is_error: false,
                });
            }
            AgentEvent::ToolResult {
                id,
                output,
                is_error,
            } => {
                let found = self.messages.iter_mut().rev().find_map(|m| match m {
                    Message::Tool {
                        id: tid,
                        output: o,
                        is_error: e,
                        ..
                    } if *tid == id => Some((o, e)),
                    _ => None,
                });
                if let Some((o, e)) = found {
                    *o = Some(truncate(&output, 4000));
                    *e = is_error;
                }
            }
            AgentEvent::Done {
                is_error,
                message,
                cost_usd,
            } => {
                self.flush(streaming);
                if is_error {
                    let text = message
                        .filter(|m| !m.trim().is_empty())
                        .unwrap_or_else(|| "The agent stopped with an error".to_owned());
                    self.messages.push(Message::Error { text });
                } else if let Some(cost) = cost_usd.filter(|c| *c > 0.0) {
                    self.messages.push(Message::Info {
                        text: format!("${cost:.4}"),
                    });
                }
            }
            AgentEvent::Error(text) => {
                self.flush(streaming);
                self.messages.push(Message::Error { text });
            }
        }
    }

    /// Moves streamed text into the transcript.
    pub fn flush(&mut self, streaming: &mut String) {
        let text = std::mem::take(streaming);
        if !text.trim().is_empty() {
            self.push_text(text);
        }
    }

    fn push_text(&mut self, text: String) {
        self.messages.push(Message::Assistant { text });
    }

    /// Names the thread after its first message.
    pub fn set_title_from(&mut self, prompt: &str) {
        if self
            .messages
            .iter()
            .any(|m| matches!(m, Message::User { .. }))
        {
            return;
        }
        let title = super::provider::first_line(prompt, 60);
        if !title.is_empty() {
            self.title = title;
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &text[..end])
}

// ---- persistence -----------------------------------------------------------

fn store_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FORGE_AI_THREADS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    crate::theme::config_dir().map(|d| d.join("ai-threads.json"))
}

/// Every saved thread, for all folders.
pub fn load_all() -> Vec<Thread> {
    let Some(path) = store_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_all(threads: &[Thread]) -> Result<(), String> {
    let path = store_path().ok_or("no config folder")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string(threads).map_err(|e| e.to_string())?;
    // Write then rename so a crash never leaves a half-written file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

// ---- running a turn --------------------------------------------------------

/// Output of a running agent process.
pub enum RunEvent {
    Agent(AgentEvent),
    /// The process exited; `stderr` is its tail when it failed.
    Exited {
        success: bool,
        stderr: String,
    },
}

/// One turn of an agent: a CLI process and its parsed output.
pub struct Run {
    pub rx: Receiver<RunEvent>,
    child: Arc<Mutex<Option<Child>>>,
    pub started: std::time::Instant,
}

impl Run {
    pub fn start(
        ctx: &egui::Context,
        exe: &Path,
        provider: Provider,
        cwd: &Path,
        args: Vec<String>,
        stdin: Option<String>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(exe);
        cmd.args(&args)
            .current_dir(cwd)
            .env("PATH", augmented_path(exe))
            .env("NO_COLOR", "1")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut cmd);
        // Its own process group, so Stop also ends what the CLI started.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            std::thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            });
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(Some(child)));
        let (tx, rx) = channel();

        let err_buf = Arc::new(Mutex::new(String::new()));
        let err_thread = stderr.map(|mut pipe| {
            let buf = err_buf.clone();
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = pipe.read_to_string(&mut text);
                *buf.lock().unwrap() = text;
            })
        });

        let ctx = ctx.clone();
        let waiter = child.clone();
        std::thread::spawn(move || {
            if let Some(out) = stdout {
                for line in BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    for ev in provider.parse_line(&line) {
                        if tx.send(RunEvent::Agent(ev)).is_err() {
                            return;
                        }
                    }
                    ctx.request_repaint();
                }
            }
            if let Some(t) = err_thread {
                let _ = t.join();
            }
            let status = waiter
                .lock()
                .unwrap()
                .take()
                .and_then(|mut c| c.wait().ok());
            let stderr = err_buf.lock().unwrap().clone();
            let tail: String = {
                let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
                lines[lines.len().saturating_sub(12)..].join("\n")
            };
            let _ = tx.send(RunEvent::Exited {
                success: status.is_some_and(|s| s.success()),
                stderr: tail,
            });
            ctx.request_repaint();
        });
        Ok(Self {
            rx,
            child,
            started: std::time::Instant::now(),
        })
    }

    /// Stops the process; its reader then reports the exit.
    pub fn stop(&self) {
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            #[cfg(unix)]
            {
                let _ = Command::new("kill")
                    .args(["-TERM", &format!("-{}", child.id())])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_text_is_replaced_by_full_message() {
        let mut t = Thread::new(
            PathBuf::from("/x"),
            ModelChoice::new(Provider::Claude, ""),
            Access::Full,
        );
        let mut buf = String::new();
        t.apply(AgentEvent::TextDelta("Hel".into()), &mut buf);
        t.apply(AgentEvent::TextDelta("lo".into()), &mut buf);
        assert_eq!(buf, "Hello");
        t.apply(AgentEvent::Text("Hello!".into()), &mut buf);
        assert!(buf.is_empty());
        assert_eq!(
            t.messages,
            vec![Message::Assistant {
                text: "Hello!".into()
            }]
        );
    }

    #[test]
    fn deltas_flush_before_tools_and_results_attach() {
        let mut t = Thread::new(
            PathBuf::from("/x"),
            ModelChoice::new(Provider::Cursor, "auto"),
            Access::Full,
        );
        let mut buf = String::new();
        t.apply(AgentEvent::TextDelta("Looking".into()), &mut buf);
        t.apply(
            AgentEvent::Tool {
                id: "1".into(),
                name: "read".into(),
                detail: "a.rs".into(),
            },
            &mut buf,
        );
        t.apply(
            AgentEvent::ToolResult {
                id: "1".into(),
                output: "fn main".into(),
                is_error: false,
            },
            &mut buf,
        );
        assert_eq!(t.messages.len(), 2);
        assert!(matches!(&t.messages[1], Message::Tool { output: Some(o), .. } if o == "fn main"));
    }

    #[test]
    fn title_from_first_prompt_only() {
        let mut t = Thread::new(
            PathBuf::from("/x"),
            ModelChoice::new(Provider::Claude, ""),
            Access::Full,
        );
        t.set_title_from("\n  Fix the build\nplease");
        assert_eq!(t.title, "Fix the build");
        t.messages.push(Message::User { text: "x".into() });
        t.set_title_from("Other");
        assert_eq!(t.title, "Fix the build");
    }
}

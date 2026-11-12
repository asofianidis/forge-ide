//! The agent CLIs Forge can drive: how to find them, which models and
//! reasoning levels they offer, how to start a turn, and how to read the
//! JSON lines they stream back.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lsp::detect::find_executable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Cursor,
    Codex,
}

/// One entry in a provider's built-in model list.
#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    /// What is passed to the CLI's `--model`; empty means "the CLI's default".
    pub id: &'static str,
    pub name: &'static str,
    /// Listed only after "+ more".
    pub more: bool,
}

const fn model(id: &'static str, name: &'static str) -> ModelInfo {
    ModelInfo {
        id,
        name,
        more: false,
    }
}

const fn more(id: &'static str, name: &'static str) -> ModelInfo {
    ModelInfo {
        id,
        name,
        more: true,
    }
}

const CLAUDE_MODELS: &[ModelInfo] = &[
    model("claude-opus-5-5", "Claude Opus 5.5"),
    model("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    model("claude-fable-5-2", "Claude Fable 5.2"),
    model("claude-haiku-5-5", "Claude Haiku 5.5"),
    more("opus", "Opus (latest)"),
    more("sonnet", "Sonnet (latest)"),
    more("fable", "Fable (latest)"),
    more("haiku", "Haiku (latest)"),
    more("", "Default (Claude config)"),
];

/// Used until `cursor-agent models` answers (or if it can't). Cursor puts
/// the reasoning level in the model id, so each level is its own entry.
const CURSOR_MODELS: &[ModelInfo] = &[
    model("auto", "Auto"),
    model("composer-2", "Composer 2"),
    model("composer-1.5", "Composer 1.5"),
    model("sonnet-4.6", "Claude Sonnet 4.6"),
    model("sonnet-4.6-thinking", "Claude Sonnet 4.6 Thinking"),
    model("opus-4.6", "Claude Opus 4.6"),
    model("opus-4.6-thinking", "Claude Opus 4.6 Thinking"),
    more("opus-4.5", "Claude Opus 4.5"),
    more("opus-4.5-thinking", "Claude Opus 4.5 Thinking"),
];

/// Used until Codex's own model cache is found.
const CODEX_MODELS: &[ModelInfo] = &[
    model("gpt-6-astra", "GPT-6 Astra"),
    model("gpt-6.1-sol", "GPT-6.1 Sol"),
    model("gpt-6-luna", "GPT-6 Luna"),
    more("gpt-6-sol", "GPT-6 Sol"),
    more("gpt-5.6-sol", "GPT-5.6 Sol"),
    more("gpt-5.6-terra", "GPT-5.6 Terra"),
    more("gpt-5.6-luna", "GPT-5.6 Luna"),
    more("gpt-5.5", "GPT-5.5"),
    more("gpt-5.4-mini", "GPT-5.4 Mini"),
    more("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
    more("", "Default (Codex config)"),
];

/// Reasoning levels: (value passed to the CLI, label).
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const CODEX_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// A model as the picker shows it: from the CLI itself when it could be
/// asked, otherwise from the built-in list.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub more: bool,
    /// Reasoning levels this model takes, when the CLI said so.
    pub efforts: Option<Vec<String>>,
    pub default_effort: Option<String>,
}

impl From<&ModelInfo> for Model {
    fn from(m: &ModelInfo) -> Self {
        Self {
            id: m.id.to_owned(),
            name: m.name.to_owned(),
            more: m.more,
            efforts: None,
            default_effort: None,
        }
    }
}

/// Model lists the CLIs reported, by provider.
static DISCOVERED: std::sync::RwLock<Vec<(Provider, Vec<Model>)>> =
    std::sync::RwLock::new(Vec::new());

/// Label for a reasoning level.
pub fn effort_name(effort: &str) -> String {
    match effort {
        "minimal" => "Minimal".to_owned(),
        "low" => "Low".to_owned(),
        "medium" => "Medium".to_owned(),
        "high" => "High".to_owned(),
        "xhigh" => "Extra High".to_owned(),
        "max" => "Max".to_owned(),
        "none" => "None".to_owned(),
        other => {
            let mut c = other.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    }
}

/// Asks each installed CLI for its models, in the background.
pub fn refresh_catalogs(ctx: &egui::Context, installed: Vec<(Provider, PathBuf)>) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        for (p, exe) in installed {
            if let Some(models) = discover(p, &exe).filter(|m| !m.is_empty()) {
                let mut store = DISCOVERED.write().unwrap();
                store.retain(|(x, _)| *x != p);
                store.push((p, models));
                ctx.request_repaint();
            }
        }
    });
}

fn discover(p: Provider, exe: &Path) -> Option<Vec<Model>> {
    match p {
        // Claude's aliases and ids are stable; the built-in list is right.
        Provider::Claude => None,
        Provider::Cursor => ["models", "--list-models"].iter().find_map(|arg| {
            let out = run_with_timeout(exe, &[arg], std::time::Duration::from_secs(20))?;
            let models = parse_cursor_models(&out);
            (!models.is_empty()).then_some(models)
        }),
        Provider::Codex => {
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
            let text = std::fs::read_to_string(home.join("models_cache.json")).ok()?;
            let mut models = parse_codex_models(&text);
            if models.is_empty() {
                return None;
            }
            models.push(Model::from(&more("", "Default (Codex config)")));
            Some(models)
        }
    }
}

/// Runs `exe args` and returns stdout, or `None` if it fails or hangs.
fn run_with_timeout(exe: &Path, args: &[&str], limit: std::time::Duration) -> Option<String> {
    use std::io::Read as _;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .env("PATH", augmented_path(exe))
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    crate::lsp::detect::hide_console_window(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if start.elapsed() < limit => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Removes terminal color codes.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Reads `cursor-agent models` output: one `id - Display Name` per line,
/// the current or default model marked in parentheses.
pub fn parse_cursor_models(text: &str) -> Vec<Model> {
    let text = strip_ansi(text);
    let mut out: Vec<Model> = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['*', '-', '•', '>']).trim();
        let Some((id, name)) = line.split_once(" - ").or_else(|| line.split_once(" – ")) else {
            continue;
        };
        let id = id.trim();
        let valid = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/'));
        if !valid || out.iter().any(|m| m.id == id) {
            continue;
        }
        let mut name = name.trim().to_owned();
        for tag in ["(current)", "(default)", "(selected)"] {
            name = name.replace(tag, "");
        }
        let name = name.trim();
        out.push(Model {
            id: id.to_owned(),
            name: if name.is_empty() {
                id.to_owned()
            } else {
                name.to_owned()
            },
            more: false,
            efforts: None,
            default_effort: None,
        });
    }
    out
}

/// Reads Codex's `models_cache.json`: the models the account can use, each
/// with the reasoning levels it supports.
pub fn parse_codex_models(text: &str) -> Vec<Model> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let list = v
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| v.as_array());
    let mut out = Vec::new();
    for m in list.into_iter().flatten() {
        let id = ["slug", "id", "model"]
            .iter()
            .find_map(|k| m.get(*k).and_then(Value::as_str))
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        if let Some(vis) = m.get("visibility").and_then(Value::as_str)
            && vis != "list"
        {
            continue;
        }
        let name = ["display_name", "displayName", "name"]
            .iter()
            .find_map(|k| m.get(*k).and_then(Value::as_str))
            .unwrap_or(id);
        let efforts: Vec<String> = m
            .get("supported_reasoning_levels")
            .or_else(|| m.get("supportedReasoningEfforts"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                e.as_str()
                    .or_else(|| e.get("effort").and_then(Value::as_str))
                    .or_else(|| e.get("reasoningEffort").and_then(Value::as_str))
                    .map(str::to_owned)
            })
            .collect();
        let default_effort = ["default_reasoning_level", "defaultReasoningEffort"]
            .iter()
            .find_map(|k| m.get(*k).and_then(Value::as_str))
            .map(str::to_owned);
        out.push(Model {
            id: id.to_owned(),
            name: name.to_owned(),
            more: false,
            efforts: (!efforts.is_empty()).then_some(efforts),
            default_effort,
        });
    }
    out
}

/// How much the agent may do without asking (the CLIs run headless, so
/// nothing can ask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    /// Edit files and run commands.
    #[default]
    Full,
    /// Edit files in the folder; commands are refused or sandboxed.
    Edits,
    /// Read and answer only.
    ReadOnly,
}

impl Access {
    pub const ALL: [Access; 3] = [Access::Full, Access::Edits, Access::ReadOnly];

    pub fn label(self) -> &'static str {
        match self {
            Access::Full => "Full access",
            Access::Edits => "Edit files only",
            Access::ReadOnly => "Read only",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Access::Full => "Edit files and run any command without asking",
            Access::Edits => "Edit files in the folder; shell commands are blocked or sandboxed",
            Access::ReadOnly => "Read files and answer; no edits or commands",
        }
    }
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Claude, Provider::Cursor, Provider::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Cursor => "Cursor",
            Provider::Codex => "Codex",
        }
    }

    /// Executable names to look for, in order.
    fn executables(self) -> &'static [&'static str] {
        match self {
            Provider::Claude => &["claude"],
            // The Cursor CLI installs as `cursor-agent`, newer builds also as `agent`.
            Provider::Cursor => &["cursor-agent", "agent"],
            Provider::Codex => &["codex"],
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Provider::Claude => "npm install -g @anthropic-ai/claude-code",
            Provider::Cursor => "curl https://cursor.com/install -fsS | bash",
            Provider::Codex => "npm install -g @openai/codex",
        }
    }

    /// Finds the CLI on this machine.
    pub fn locate(self) -> Option<PathBuf> {
        self.executables().iter().find_map(|n| find_executable(n))
    }

    fn builtin_models(self) -> &'static [ModelInfo] {
        match self {
            Provider::Claude => CLAUDE_MODELS,
            Provider::Cursor => CURSOR_MODELS,
            Provider::Codex => CODEX_MODELS,
        }
    }

    /// The models to offer: what the CLI reported, else the built-in list.
    pub fn models(self) -> Vec<Model> {
        if let Some((_, list)) = DISCOVERED.read().unwrap().iter().find(|(p, _)| *p == self) {
            return list.clone();
        }
        self.builtin_models().iter().map(Model::from).collect()
    }

    /// Reasoning levels `model` takes, as (value, label). Empty for Cursor,
    /// whose models carry the level in their id.
    pub fn efforts_for(self, model: &str) -> Vec<(String, String)> {
        let base: &[&str] = match self {
            Provider::Claude => CLAUDE_EFFORTS,
            Provider::Codex => CODEX_EFFORTS,
            Provider::Cursor => return Vec::new(),
        };
        let reported = self
            .models()
            .into_iter()
            .find(|m| m.id == model)
            .and_then(|m| m.efforts);
        match reported {
            Some(list) => list.iter().map(|e| (e.clone(), effort_name(e))).collect(),
            None => base
                .iter()
                .map(|e| ((*e).to_owned(), effort_name(e)))
                .collect(),
        }
    }

    /// The level a new choice of `model` starts at.
    pub fn default_effort_for(self, model: &str) -> String {
        if self == Provider::Cursor {
            return String::new();
        }
        let reported = self
            .models()
            .into_iter()
            .find(|m| m.id == model)
            .and_then(|m| m.default_effort);
        let efforts = self.efforts_for(model);
        reported
            .filter(|d| efforts.iter().any(|(v, _)| v == d))
            .or_else(|| {
                efforts
                    .iter()
                    .any(|(v, _)| v == "medium")
                    .then(|| "medium".to_owned())
            })
            .or_else(|| efforts.first().map(|(v, _)| v.clone()))
            .unwrap_or_default()
    }

    /// A display name for `model` (falls back to the id itself).
    pub fn model_name(self, model: &str) -> String {
        self.models()
            .into_iter()
            .find(|m| m.id == model)
            .map(|m| m.name)
            .unwrap_or_else(|| model.to_owned())
    }

    /// The label for `effort` on `model`, if the model takes it.
    pub fn effort_label(self, model: &str, effort: &str) -> Option<String> {
        self.efforts_for(model)
            .into_iter()
            .find(|(v, _)| v == effort)
            .map(|(_, l)| l)
    }

    /// Arguments for one agent turn. `session` resumes an earlier
    /// conversation. Returns the arguments and the text to write to stdin.
    pub fn agent_args(
        self,
        cwd: &Path,
        model: &str,
        effort: &str,
        access: Access,
        session: Option<&str>,
        prompt: &str,
    ) -> (Vec<String>, Option<String>) {
        let mut a: Vec<String> = Vec::new();
        let mut push = |items: &[&str]| a.extend(items.iter().map(|s| (*s).to_owned()));
        match self {
            Provider::Claude => {
                push(&[
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                ]);
                if !model.is_empty() {
                    push(&["--model", model]);
                }
                if !effort.is_empty() {
                    push(&["--effort", effort]);
                }
                push(&[
                    "--permission-mode",
                    match access {
                        Access::Full => "bypassPermissions",
                        Access::Edits => "acceptEdits",
                        Access::ReadOnly => "plan",
                    },
                ]);
                if let Some(id) = session {
                    push(&["--resume", id]);
                }
                (a, Some(prompt.to_owned()))
            }
            Provider::Cursor => {
                push(&[
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--stream-partial-output",
                ]);
                if !model.is_empty() {
                    push(&["--model", model]);
                }
                match access {
                    Access::Full => push(&["--force"]),
                    Access::Edits => {}
                    Access::ReadOnly => push(&["--mode", "ask"]),
                }
                if let Some(id) = session {
                    push(&["--resume", id]);
                }
                push(&["--workspace", &cwd.to_string_lossy(), "--", prompt]);
                (a, None)
            }
            Provider::Codex => {
                push(&["exec", "--json", "--skip-git-repo-check"]);
                push(&["-C", &cwd.to_string_lossy()]);
                if !model.is_empty() {
                    push(&["-m", model]);
                }
                if !effort.is_empty() {
                    push(&["-c", &format!("model_reasoning_effort=\"{effort}\"")]);
                }
                match access {
                    Access::Full => push(&["--dangerously-bypass-approvals-and-sandbox"]),
                    Access::Edits => push(&["--sandbox", "workspace-write"]),
                    Access::ReadOnly => push(&["--sandbox", "read-only"]),
                }
                if let Some(id) = session {
                    push(&["resume", id]);
                }
                push(&["--", prompt]);
                (a, None)
            }
        }
    }

    /// Arguments for a one-shot, tool-free text completion. `out_file` is
    /// where Codex writes its final message. Returns args and stdin.
    pub fn text_args(
        self,
        cwd: &Path,
        model: &str,
        effort: &str,
        prompt: &str,
        out_file: &Path,
    ) -> (Vec<String>, Option<String>) {
        let mut a: Vec<String> = Vec::new();
        let mut push = |items: &[&str]| a.extend(items.iter().map(|s| (*s).to_owned()));
        match self {
            Provider::Claude => {
                push(&[
                    "-p",
                    "--output-format",
                    "text",
                    "--tools",
                    "",
                    "--no-session-persistence",
                    "--strict-mcp-config",
                ]);
                if !model.is_empty() {
                    push(&["--model", model]);
                }
                if !effort.is_empty() {
                    push(&["--effort", effort]);
                }
                (a, Some(prompt.to_owned()))
            }
            Provider::Cursor => {
                push(&["-p", "--output-format", "text", "--mode", "ask"]);
                if !model.is_empty() {
                    push(&["--model", model]);
                }
                push(&["--workspace", &cwd.to_string_lossy(), "--", prompt]);
                (a, None)
            }
            Provider::Codex => {
                push(&["exec", "--skip-git-repo-check", "--sandbox", "read-only"]);
                push(&["-C", &cwd.to_string_lossy()]);
                push(&["--output-last-message", &out_file.to_string_lossy()]);
                if !model.is_empty() {
                    push(&["-m", model]);
                }
                if !effort.is_empty() {
                    push(&["-c", &format!("model_reasoning_effort=\"{effort}\"")]);
                }
                push(&["--", prompt]);
                (a, None)
            }
        }
    }

    /// Reads one line of the CLI's JSON stream.
    pub fn parse_line(self, line: &str) -> Vec<AgentEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        match self {
            Provider::Claude => parse_claude(&v),
            Provider::Cursor => parse_cursor(&v),
            Provider::Codex => parse_codex(&v),
        }
    }
}

/// Something that happened during an agent turn, the same for every CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// The CLI's conversation id, used to resume the next turn.
    Session(String),
    /// More of the reply currently being written.
    TextDelta(String),
    /// A complete piece of the reply (replaces any streamed deltas).
    Text(String),
    /// The agent started using a tool.
    Tool {
        id: String,
        name: String,
        detail: String,
    },
    /// A tool finished.
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// The turn ended.
    Done {
        is_error: bool,
        message: Option<String>,
        cost_usd: Option<f64>,
    },
    Error(String),
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A short description of a tool's input: its command, path or pattern.
fn tool_detail(input: &Value) -> String {
    for key in [
        "command",
        "file_path",
        "path",
        "notebook_path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
    ] {
        if let Some(text) = input.get(key).and_then(Value::as_str)
            && !text.is_empty()
        {
            return first_line(text, 200);
        }
    }
    String::new()
}

/// The first line of `text`, cut to `max` characters.
pub fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() > max {
        line.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    } else {
        line.to_owned()
    }
}

fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(t) => t.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse_claude(v: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match s(v, "type") {
        "system" if s(v, "subtype") == "init" => {
            let id = s(v, "session_id");
            if !id.is_empty() {
                out.push(AgentEvent::Session(id.to_owned()));
            }
        }
        "stream_event" => {
            let ev = &v["event"];
            // Only the top-level conversation, not subagents.
            if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
                return out;
            }
            if s(ev, "type") == "content_block_delta" && s(&ev["delta"], "type") == "text_delta" {
                out.push(AgentEvent::TextDelta(s(&ev["delta"], "text").to_owned()));
            }
        }
        "assistant" => {
            if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
                return out;
            }
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                match s(block, "type") {
                    "text" => {
                        let text = s(block, "text");
                        if !text.trim().is_empty() {
                            out.push(AgentEvent::Text(text.to_owned()));
                        }
                    }
                    "tool_use" => out.push(AgentEvent::Tool {
                        id: s(block, "id").to_owned(),
                        name: s(block, "name").to_owned(),
                        detail: tool_detail(&block["input"]),
                    }),
                    _ => {}
                }
            }
        }
        "user" => {
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                if s(block, "type") == "tool_result" {
                    out.push(AgentEvent::ToolResult {
                        id: s(block, "tool_use_id").to_owned(),
                        output: tool_result_text(&block["content"]),
                        is_error: block["is_error"].as_bool().unwrap_or(false),
                    });
                }
            }
        }
        "result" => {
            let is_error = v["is_error"].as_bool().unwrap_or(false) || s(v, "subtype") != "success";
            let message = v
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    v["errors"].as_array().map(|e| {
                        e.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                });
            out.push(AgentEvent::Done {
                is_error,
                message,
                cost_usd: v["total_cost_usd"].as_f64(),
            });
        }
        _ => {}
    }
    out
}

fn parse_cursor(v: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match s(v, "type") {
        "system" if s(v, "subtype") == "init" => {
            let id = s(v, "session_id");
            if !id.is_empty() {
                out.push(AgentEvent::Session(id.to_owned()));
            }
        }
        "assistant" => {
            let text: String = v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| s(b, "type") == "text")
                .map(|b| s(b, "text"))
                .collect();
            if !text.is_empty() {
                // With --stream-partial-output each event is a fragment.
                out.push(AgentEvent::TextDelta(text));
            }
        }
        "tool_call" => {
            let id = s(v, "call_id").to_owned();
            // `{"shellToolCall": {"args": {...}, "result": {...}}}`
            let Some((kind, call)) = v["tool_call"].as_object().and_then(|o| o.iter().next())
            else {
                return out;
            };
            let name = kind.strip_suffix("ToolCall").unwrap_or(kind).to_owned();
            let name = if name == "function" {
                s(call, "name").to_owned()
            } else {
                name
            };
            match s(v, "subtype") {
                "started" => out.push(AgentEvent::Tool {
                    id,
                    name,
                    detail: tool_detail(&call["args"]),
                }),
                "completed" => {
                    let result = &call["result"];
                    let is_error = result.get("error").is_some()
                        || result.get("failure").is_some()
                        || result.get("rejected").is_some();
                    let output = result
                        .get("success")
                        .map(|r| {
                            ["stdout", "content", "output"]
                                .iter()
                                .find_map(|k| r.get(*k).and_then(Value::as_str))
                                .unwrap_or_default()
                                .to_owned()
                        })
                        .unwrap_or_else(|| {
                            result
                                .get("error")
                                .map(|e| match e {
                                    Value::String(t) => t.clone(),
                                    other => s(other, "message").to_owned(),
                                })
                                .unwrap_or_default()
                        });
                    out.push(AgentEvent::ToolResult {
                        id,
                        output,
                        is_error,
                    });
                }
                _ => {}
            }
        }
        "result" => {
            let is_error = v["is_error"].as_bool().unwrap_or(false) || s(v, "subtype") != "success";
            out.push(AgentEvent::Done {
                is_error,
                // The text already arrived as deltas.
                message: is_error.then(|| s(v, "result").to_owned()),
                cost_usd: None,
            });
        }
        _ => {}
    }
    out
}

fn parse_codex(v: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let item = &v["item"];
    match s(v, "type") {
        "thread.started" => {
            let id = s(v, "thread_id");
            if !id.is_empty() {
                out.push(AgentEvent::Session(id.to_owned()));
            }
        }
        "item.started" => match s(item, "type") {
            "command_execution" => out.push(AgentEvent::Tool {
                id: s(item, "id").to_owned(),
                name: "Shell".to_owned(),
                detail: first_line(s(item, "command"), 200),
            }),
            "mcp_tool_call" => out.push(AgentEvent::Tool {
                id: s(item, "id").to_owned(),
                name: format!("{}.{}", s(item, "server"), s(item, "tool")),
                detail: String::new(),
            }),
            "web_search" => out.push(AgentEvent::Tool {
                id: s(item, "id").to_owned(),
                name: "Web search".to_owned(),
                detail: s(item, "query").to_owned(),
            }),
            _ => {}
        },
        "item.completed" => {
            let id = s(item, "id").to_owned();
            match s(item, "type") {
                "agent_message" => out.push(AgentEvent::Text(s(item, "text").to_owned())),
                "command_execution" | "mcp_tool_call" | "web_search" => {
                    let failed = s(item, "status") == "failed"
                        || item["exit_code"].as_i64().is_some_and(|c| c != 0);
                    out.push(AgentEvent::ToolResult {
                        id,
                        output: s(item, "aggregated_output").to_owned(),
                        is_error: failed,
                    });
                }
                "file_change" => {
                    let files: Vec<String> = item["changes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| format!("{} {}", s(c, "kind"), s(c, "path")))
                        .collect();
                    out.push(AgentEvent::Tool {
                        id: id.clone(),
                        name: "Edit".to_owned(),
                        detail: files.join(", "),
                    });
                    out.push(AgentEvent::ToolResult {
                        id,
                        output: String::new(),
                        is_error: s(item, "status") == "failed",
                    });
                }
                "error" => out.push(AgentEvent::Error(s(item, "message").to_owned())),
                _ => {}
            }
        }
        "turn.completed" => out.push(AgentEvent::Done {
            is_error: false,
            message: None,
            cost_usd: None,
        }),
        "turn.failed" => out.push(AgentEvent::Done {
            is_error: true,
            message: Some(s(&v["error"], "message").to_owned()),
            cost_usd: None,
        }),
        "error" => out.push(AgentEvent::Error(s(v, "message").to_owned())),
        _ => {}
    }
    out
}

/// PATH for the CLIs: the current one plus the usual install folders, since
/// apps launched from the Finder or a desktop launcher get a minimal PATH and
/// these CLIs often run on `node`.
pub fn augmented_path(exe: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe.parent() {
        dirs.push(dir.to_path_buf());
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(home) = dirs::home_dir() {
        for sub in [
            ".local/bin",
            ".cargo/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".volta/bin",
        ] {
            dirs.push(home.join(sub));
        }
    }
    if !cfg!(windows) {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    std::env::join_paths(dirs).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_stream() {
        let p = Provider::Claude;
        assert_eq!(
            p.parse_line(r#"{"type":"system","subtype":"init","session_id":"abc"}"#),
            vec![AgentEvent::Session("abc".into())]
        );
        assert_eq!(
            p.parse_line(
                r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}}"#
            ),
            vec![AgentEvent::TextDelta("Hi".into())]
        );
        assert_eq!(
            p.parse_line(
                r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls -la\nmore"}}]}}"#
            ),
            vec![AgentEvent::Tool {
                id: "t1".into(),
                name: "Bash".into(),
                detail: "ls -la".into()
            }]
        );
        assert_eq!(
            p.parse_line(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}]}}"#
            ),
            vec![AgentEvent::ToolResult {
                id: "t1".into(),
                output: "ok".into(),
                is_error: false
            }]
        );
        assert_eq!(
            p.parse_line(
                r#"{"type":"result","subtype":"success","is_error":false,"result":"done","total_cost_usd":0.01}"#
            ),
            vec![AgentEvent::Done {
                is_error: false,
                message: Some("done".into()),
                cost_usd: Some(0.01)
            }]
        );
        assert!(p.parse_line("not json").is_empty());
    }

    #[test]
    fn codex_stream() {
        let p = Provider::Codex;
        assert_eq!(
            p.parse_line(r#"{"type":"thread.started","thread_id":"th"}"#),
            vec![AgentEvent::Session("th".into())]
        );
        assert_eq!(
            p.parse_line(
                r#"{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"hello"}}"#
            ),
            vec![AgentEvent::Text("hello".into())]
        );
        assert_eq!(
            p.parse_line(
                r#"{"type":"item.started","item":{"id":"c","type":"command_execution","command":"bash -lc ls"}}"#
            ),
            vec![AgentEvent::Tool {
                id: "c".into(),
                name: "Shell".into(),
                detail: "bash -lc ls".into()
            }]
        );
        assert_eq!(
            p.parse_line(r#"{"type":"turn.failed","error":{"message":"nope"}}"#),
            vec![AgentEvent::Done {
                is_error: true,
                message: Some("nope".into()),
                cost_usd: None
            }]
        );
    }

    #[test]
    fn cursor_stream() {
        let p = Provider::Cursor;
        assert_eq!(
            p.parse_line(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hey"}]},"session_id":"s"}"#
            ),
            vec![AgentEvent::TextDelta("Hey".into())]
        );
        assert_eq!(
            p.parse_line(
                r#"{"type":"tool_call","subtype":"started","call_id":"c1","tool_call":{"readToolCall":{"args":{"path":"src/a.rs"}}}}"#
            ),
            vec![AgentEvent::Tool {
                id: "c1".into(),
                name: "read".into(),
                detail: "src/a.rs".into()
            }]
        );
    }

    #[test]
    fn cursor_model_list_parses() {
        let out = "\x1b[1mAvailable models\x1b[0m\n\nauto - Auto  (current)\ncomposer-2 - Composer 2\ngpt-5.3-codex-high - GPT-5.3 Codex High\nsonnet-4.6-thinking - Claude Sonnet 4.6 Thinking (default)\n\nTip: use --model <id>\n";
        let m = parse_cursor_models(out);
        let ids: Vec<&str> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "auto",
                "composer-2",
                "gpt-5.3-codex-high",
                "sonnet-4.6-thinking"
            ]
        );
        assert_eq!(m[0].name, "Auto");
        assert_eq!(m[3].name, "Claude Sonnet 4.6 Thinking");
        assert!(m.iter().all(|m| m.efforts.is_none()));
    }

    #[test]
    fn codex_model_cache_parses() {
        let json = r#"{"fetched_at":"x","models":[
            {"slug":"gpt-6-astra","display_name":"GPT-6 Astra","visibility":"list",
             "default_reasoning_level":"medium",
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"},{"effort":"xhigh"}]},
            {"slug":"internal-thing","display_name":"Hidden","visibility":"hide"},
            {"slug":"gpt-6-luna","display_name":"GPT-6 Luna","visibility":"list",
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"}]}]}"#;
        let m = parse_codex_models(json);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "GPT-6 Astra");
        assert_eq!(m[0].default_effort.as_deref(), Some("medium"));
        assert_eq!(m[1].efforts.as_ref().unwrap(), &["low", "medium"]);
        assert!(parse_codex_models("not json").is_empty());
    }

    #[test]
    fn cursor_has_no_effort_selector_and_codex_does() {
        assert!(
            Provider::Cursor
                .efforts_for("gpt-5.3-codex-high")
                .is_empty()
        );
        assert_eq!(Provider::Cursor.default_effort_for("auto"), "");
        let codex: Vec<String> = Provider::Codex
            .efforts_for("gpt-6-astra")
            .into_iter()
            .map(|(_, l)| l)
            .collect();
        assert_eq!(codex, ["Low", "Medium", "High", "Extra High"]);
        assert_eq!(Provider::Codex.default_effort_for("gpt-6-astra"), "medium");
        assert_eq!(effort_name("xhigh"), "Extra High");
    }

    #[test]
    fn agent_args_resume_and_access() {
        let cwd = Path::new("/tmp/x");
        let (args, stdin) = Provider::Claude.agent_args(
            cwd,
            "claude-opus-5-5",
            "high",
            Access::Edits,
            Some("sid"),
            "do it",
        );
        assert!(args.windows(2).any(|w| w == ["--resume", "sid"]));
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-mode", "acceptEdits"])
        );
        assert_eq!(stdin.as_deref(), Some("do it"));

        let (args, stdin) =
            Provider::Codex.agent_args(cwd, "", "low", Access::ReadOnly, Some("th"), "-x");
        assert!(stdin.is_none());
        assert!(!args.iter().any(|a| a == "-m"));
        let resume = args.iter().position(|a| a == "resume").unwrap();
        assert_eq!(args[resume + 1], "th");
        assert_eq!(&args[args.len() - 2..], ["--", "-x"]);
    }
}

//! Language Server Protocol client.
//!
//! One server process is started per configured command, lazily, the first
//! time a file it handles becomes active. Documents are synced with full-text
//! `didChange` notifications. Responses arrive on a background thread and are
//! drained once per frame by [`LspManager::poll`].

mod convert;
mod transport;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lsp_types::{
    CompletionItem, CompletionResponse, Diagnostic, GotoDefinitionResponse, Position,
    PublishDiagnosticsParams, Range,
};
use serde_json::{Value, json};

use convert::{language_id, path_to_uri, uri_to_path};
pub use convert::{normalize, strip_snippet};
use transport::{Connection, Incoming};

use crate::editor::Document;
use crate::settings::ServerConfig;

/// Minimum time between full-text syncs while typing.
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);

pub enum LspEvent {
    Diagnostics {
        path: PathBuf,
        diagnostics: Vec<Diagnostic>,
    },
    Completion {
        path: PathBuf,
        items: Vec<CompletionItem>,
    },
    Definition(Vec<(PathBuf, Range)>),
    /// Something worth showing in the status bar.
    Status(String),
}

enum Pending {
    Initialize,
    Completion { path: PathBuf },
    Definition,
    Ignore,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    Starting,
    Ready,
    Exited,
}

struct OpenDoc {
    version: i32,
    revision: u64,
    last_sent: Instant,
}

struct Server {
    name: String,
    conn: Connection,
    state: ServerState,
    next_id: i64,
    pending: HashMap<i64, Pending>,
    /// Messages held back until the `initialize` handshake completes.
    queue: Vec<Value>,
    trigger_chars: Vec<String>,
    docs: HashMap<PathBuf, OpenDoc>,
    /// Latest `$/progress` message, e.g. "Indexing 40%".
    progress: Option<String>,
}

impl Server {
    fn start(
        config: &ServerConfig,
        root: Option<&Path>,
        ctx: &egui::Context,
    ) -> anyhow::Result<Self> {
        let conn = Connection::spawn(&config.command, &config.args, root, ctx)?;
        let mut server = Self {
            name: config.command.clone(),
            conn,
            state: ServerState::Starting,
            next_id: 0,
            pending: HashMap::new(),
            queue: Vec::new(),
            trigger_chars: Vec::new(),
            docs: HashMap::new(),
            progress: None,
        };
        let root_uri = root.map(path_to_uri);
        let folders = root.map(|r| {
            json!([{
                "uri": path_to_uri(r),
                "name": r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            }])
        });
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "forge-ide", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": root_uri,
            "workspaceFolders": folders,
            "capabilities": {
                "general": {"positionEncodings": ["utf-16"]},
                "window": {"workDoneProgress": true},
                "workspace": {"configuration": true, "workspaceFolders": true},
                "textDocument": {
                    "synchronization": {"didSave": true, "dynamicRegistration": false},
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true,
                            "insertReplaceSupport": true,
                            "labelDetailsSupport": true,
                        },
                    },
                    "definition": {"linkSupport": true},
                    "publishDiagnostics": {"relatedInformation": false},
                },
            },
        });
        // `initialize` must go out before anything else, so bypass the queue.
        let id = server.alloc_id(Pending::Initialize);
        server.conn.send(&request(id, "initialize", params))?;
        Ok(server)
    }

    fn alloc_id(&mut self, pending: Pending) -> i64 {
        self.next_id += 1;
        self.pending.insert(self.next_id, pending);
        self.next_id
    }

    fn send(&mut self, message: Value) {
        if self.state == ServerState::Starting {
            self.queue.push(message);
        } else if self.state == ServerState::Ready {
            let _ = self.conn.send(&message);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn request(&mut self, method: &str, params: Value, pending: Pending) {
        let id = self.alloc_id(pending);
        self.send(request(id, method, params));
    }

    /// Opens or updates a document on the server. With `force`, skips the debounce.
    fn sync(&mut self, path: &Path, doc: &Document, force: bool) {
        let uri = path_to_uri(path);
        let revision = doc.buffer.revision();
        match self.docs.get_mut(path) {
            None => {
                self.docs.insert(
                    path.to_path_buf(),
                    OpenDoc {
                        version: 1,
                        revision,
                        last_sent: Instant::now(),
                    },
                );
                let text = egui::TextBuffer::as_str(&doc.buffer);
                self.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {
                        "uri": uri,
                        "languageId": language_id(doc.extension()),
                        "version": 1,
                        "text": text,
                    }}),
                );
            }
            Some(open) if open.revision != revision => {
                if !force && open.last_sent.elapsed() < CHANGE_DEBOUNCE {
                    return;
                }
                open.version += 1;
                open.revision = revision;
                open.last_sent = Instant::now();
                let version = open.version;
                let text = egui::TextBuffer::as_str(&doc.buffer);
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri, "version": version},
                        "contentChanges": [{"text": text}],
                    }),
                );
            }
            Some(_) => {}
        }
    }

    /// Handles one incoming message, pushing anything the app needs to know into `events`.
    fn handle(&mut self, message: Value, events: &mut Vec<LspEvent>) {
        let id = message.get("id").cloned();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (id, method) {
            // Response to one of our requests.
            (Some(id), None) => {
                let Some(pending) = id.as_i64().and_then(|id| self.pending.remove(&id)) else {
                    return;
                };
                if let Some(err) = message.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    // Cancellations and "content modified" are routine while typing.
                    if !matches!(pending, Pending::Completion { .. }) {
                        events.push(LspEvent::Status(format!("{}: {msg}", self.name)));
                    }
                    return;
                }
                let result = message.get("result").cloned().unwrap_or(Value::Null);
                self.handle_response(pending, result, events);
            }
            // Request from the server: answer so it doesn't wait on us.
            (Some(id), Some(method)) => {
                let result = match method.as_str() {
                    "workspace/configuration" => {
                        let n = message["params"]["items"].as_array().map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; n])
                    }
                    _ => Value::Null,
                };
                let _ = self
                    .conn
                    .send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
            (None, Some(method)) => self.handle_notification(&method, &message["params"], events),
            (None, None) => {}
        }
    }

    fn handle_response(&mut self, pending: Pending, result: Value, events: &mut Vec<LspEvent>) {
        match pending {
            Pending::Initialize => {
                self.trigger_chars =
                    result["capabilities"]["completionProvider"]["triggerCharacters"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                self.state = ServerState::Ready;
                let _ = self
                    .conn
                    .send(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
                for message in std::mem::take(&mut self.queue) {
                    let _ = self.conn.send(&message);
                }
                events.push(LspEvent::Status(format!("{} ready", self.name)));
            }
            Pending::Completion { path } => {
                let items = match serde_json::from_value::<Option<CompletionResponse>>(result) {
                    Ok(Some(CompletionResponse::Array(items))) => items,
                    Ok(Some(CompletionResponse::List(list))) => list.items,
                    _ => Vec::new(),
                };
                events.push(LspEvent::Completion { path, items });
            }
            Pending::Definition => {
                let locations =
                    match serde_json::from_value::<Option<GotoDefinitionResponse>>(result) {
                        Ok(Some(GotoDefinitionResponse::Scalar(l))) => vec![(l.uri, l.range)],
                        Ok(Some(GotoDefinitionResponse::Array(ls))) => {
                            ls.into_iter().map(|l| (l.uri, l.range)).collect()
                        }
                        Ok(Some(GotoDefinitionResponse::Link(links))) => links
                            .into_iter()
                            .map(|l| (l.target_uri, l.target_selection_range))
                            .collect(),
                        _ => Vec::new(),
                    };
                let locations = locations
                    .into_iter()
                    .filter_map(|(uri, range)| Some((uri_to_path(uri.as_str())?, range)))
                    .collect();
                events.push(LspEvent::Definition(locations));
            }
            Pending::Ignore => {}
        }
    }

    fn handle_notification(&mut self, method: &str, params: &Value, events: &mut Vec<LspEvent>) {
        match method {
            "textDocument/publishDiagnostics" => {
                if let Ok(p) = serde_json::from_value::<PublishDiagnosticsParams>(params.clone())
                    && let Some(path) = uri_to_path(p.uri.as_str())
                {
                    events.push(LspEvent::Diagnostics {
                        path,
                        diagnostics: p.diagnostics,
                    });
                }
            }
            "window/showMessage" => {
                // Only errors and warnings (type 1 and 2) are worth interrupting for.
                if params["type"].as_i64().is_some_and(|t| t <= 2)
                    && let Some(msg) = params["message"].as_str()
                {
                    events.push(LspEvent::Status(format!("{}: {msg}", self.name)));
                }
            }
            "$/progress" => {
                let value = &params["value"];
                self.progress = match value["kind"].as_str() {
                    Some("end") => None,
                    _ => {
                        let title = value["title"].as_str().map(str::to_owned).or_else(|| {
                            self.progress
                                .as_ref()
                                .map(|p| p.split(" (").next().unwrap_or(p).to_owned())
                        });
                        let detail = match (value["message"].as_str(), value["percentage"].as_u64())
                        {
                            (Some(m), Some(p)) => format!(" ({m}, {p}%)"),
                            (Some(m), None) => format!(" ({m})"),
                            (None, Some(p)) => format!(" ({p}%)"),
                            (None, None) => String::new(),
                        };
                        title.map(|t| format!("{t}{detail}"))
                    }
                };
            }
            _ => {}
        }
    }
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn position(p: (u32, u32)) -> Value {
    json!({"line": p.0, "character": p.1})
}

#[derive(Default)]
pub struct LspManager {
    servers: HashMap<String, Server>,
    /// Which server each open document belongs to.
    doc_server: HashMap<PathBuf, String>,
    /// Commands that failed to start; not retried until [`Self::restart`].
    failed: HashSet<String>,
}

impl LspManager {
    /// Makes sure the document is open (and up to date) on its language server,
    /// starting the server if needed.
    pub fn sync(
        &mut self,
        doc: &Document,
        config: Option<&ServerConfig>,
        root: Option<&Path>,
        ctx: &egui::Context,
    ) -> Option<String> {
        let path = normalize(doc.path.as_deref()?);
        let config = config.filter(|c| !c.command.trim().is_empty())?;
        let key = config.command.clone();
        let mut error = None;
        if !self.servers.contains_key(&key) && !self.failed.contains(&key) {
            let root = root.or_else(|| path.parent());
            match Server::start(config, root, ctx) {
                Ok(server) => {
                    self.servers.insert(key.clone(), server);
                }
                Err(e) => {
                    self.failed.insert(key.clone());
                    error = Some(format!("{e:#}"));
                }
            }
        }
        let server = self.servers.get_mut(&key)?;
        self.doc_server.insert(path.clone(), key);
        server.sync(&path, doc, false);
        if server
            .docs
            .get(&path)
            .is_some_and(|d| d.revision != doc.buffer.revision())
        {
            // Debounced; make sure we come back to send it.
            ctx.request_repaint_after(CHANGE_DEBOUNCE);
        }
        error
    }

    fn server_for(&mut self, doc: &Document) -> Option<(PathBuf, &mut Server)> {
        let path = normalize(doc.path.as_deref()?);
        let key = self.doc_server.get(&path)?;
        let server = self.servers.get_mut(key)?;
        Some((path, server))
    }

    pub fn did_save(&mut self, doc: &Document) {
        if let Some((path, server)) = self.server_for(doc) {
            server.sync(&path, doc, true);
            server.notify(
                "textDocument/didSave",
                json!({"textDocument": {"uri": path_to_uri(&path)}}),
            );
        }
    }

    pub fn did_close(&mut self, doc: &Document) {
        if let Some((path, server)) = self.server_for(doc)
            && server.docs.remove(&path).is_some()
        {
            server.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": path_to_uri(&path)}}),
            );
        }
    }

    pub fn request_completion(&mut self, doc: &Document, char_idx: usize) -> bool {
        let Some((path, server)) = self.server_for(doc) else {
            return false;
        };
        server.sync(&path, doc, true);
        let params = json!({
            "textDocument": {"uri": path_to_uri(&path)},
            "position": position(doc.buffer.lsp_position(char_idx)),
        });
        server.request(
            "textDocument/completion",
            params,
            Pending::Completion { path },
        );
        true
    }

    pub fn request_definition(&mut self, doc: &Document, char_idx: usize) -> bool {
        let Some((path, server)) = self.server_for(doc) else {
            return false;
        };
        server.sync(&path, doc, true);
        let params = json!({
            "textDocument": {"uri": path_to_uri(&path)},
            "position": position(doc.buffer.lsp_position(char_idx)),
        });
        server.request("textDocument/definition", params, Pending::Definition);
        true
    }

    /// Characters that should automatically open completion for this document.
    pub fn trigger_characters(&self, doc: &Document) -> &[String] {
        doc.path
            .as_deref()
            .map(normalize)
            .and_then(|p| self.doc_server.get(&p))
            .and_then(|key| self.servers.get(key))
            .map_or(&[], |s| &s.trigger_chars)
    }

    /// Short status for the document's server, e.g. "rust-analyzer: Indexing (40%)".
    pub fn status(&self, doc: &Document) -> Option<String> {
        let path = normalize(doc.path.as_deref()?);
        let server = self.servers.get(self.doc_server.get(&path)?)?;
        Some(match (server.state, &server.progress) {
            (ServerState::Starting, _) => format!("{}: starting", server.name),
            (ServerState::Exited, _) => format!("{}: exited", server.name),
            (ServerState::Ready, Some(p)) => format!("{}: {p}", server.name),
            (ServerState::Ready, None) => server.name.clone(),
        })
    }

    /// Stops all servers; they restart on demand.
    pub fn restart(&mut self) {
        self.servers.clear();
        self.doc_server.clear();
        self.failed.clear();
    }

    /// Drains messages from all servers.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut events = Vec::new();
        for server in self.servers.values_mut() {
            while let Ok(incoming) = server.conn.incoming.try_recv() {
                match incoming {
                    Incoming::Message(message) => server.handle(message, &mut events),
                    Incoming::Closed(stderr) => {
                        server.state = ServerState::Exited;
                        let detail = stderr.lines().last().unwrap_or("no output");
                        events.push(LspEvent::Status(format!(
                            "{} exited: {detail}",
                            server.name
                        )));
                    }
                }
            }
        }
        events
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        for server in self.servers.values_mut() {
            if server.state == ServerState::Ready {
                let id = server.alloc_id(Pending::Ignore);
                let _ = server.conn.send(&request(id, "shutdown", Value::Null));
            }
        }
    }
}

/// Converts an LSP range to a char range in `doc`.
pub fn char_range(doc: &Document, range: &Range) -> std::ops::Range<usize> {
    let pos = |p: &Position| doc.buffer.char_from_lsp_position(p.line, p.character);
    let start = pos(&range.start);
    start..pos(&range.end).max(start)
}

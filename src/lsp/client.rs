//! One running language server: its process, the threads that move
//! messages to and from it, and the initialize handshake.
//!
//! Nothing here blocks the UI thread. Messages are written by a writer thread
//! and read by a reader thread; [`Client::poll`] picks up whatever arrived.

use std::collections::VecDeque;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lsp_types::{InitializeResult, ServerCapabilities};
use serde_json::{Value, json};

use super::catalog::ServerSpec;
use super::detect::hide_console_window;
use super::rpc::{self, Incoming, ResponseError};
use super::text::{Encoding, path_to_uri};

/// Lines of stderr kept for error reports.
const STDERR_LINES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientState {
    /// Waiting for the reply to `initialize`.
    Starting,
    Ready,
    /// The server exited, crashed or failed to initialize.
    Stopped(String),
}

pub struct Client {
    pub server: &'static ServerSpec,
    pub root: PathBuf,
    state: ClientState,
    capabilities: ServerCapabilities,
    /// How positions count characters, as negotiated during initialize.
    pub encoding: Encoding,
    next_id: i64,
    init_id: i64,
    outgoing: Sender<Value>,
    /// Messages from the reader thread; `None` when the stream closed.
    incoming: Receiver<Option<Value>>,
    /// Messages sent before the handshake finished, sent once it has.
    queued: Vec<Value>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    child: Option<Child>,
}

impl Client {
    /// Starts the server's process and begins the initialize handshake.
    pub fn spawn(
        server: &'static ServerSpec,
        program: &Path,
        root: &Path,
        ctx: &egui::Context,
    ) -> io::Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(server.launch_args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut cmd);
        let mut child = cmd.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err(io::Error::other("server has no stdio"));
        };

        let mut client = Self::connect(server, root, stdout, stdin, ctx);
        let lines = Arc::clone(&client.stderr);
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut lines = lines.lock().unwrap();
                if lines.len() == STDERR_LINES {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        });
        client.child = Some(child);
        Ok(client)
    }

    /// Talks to a server over the given streams (its stdout and stdin).
    pub fn connect(
        server: &'static ServerSpec,
        root: &Path,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        ctx: &egui::Context,
    ) -> Self {
        let (out_tx, out_rx) = channel::<Value>();
        std::thread::spawn(move || {
            let mut w = BufWriter::new(writer);
            for msg in out_rx {
                if rpc::write_message(&mut w, &msg).is_err() {
                    break;
                }
            }
        });

        let (in_tx, in_rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(reader);
            loop {
                let msg = rpc::read_message(&mut r).ok().flatten();
                let closed = msg.is_none();
                if in_tx.send(msg).is_err() || closed {
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        });

        let mut client = Self {
            server,
            root: root.to_path_buf(),
            state: ClientState::Starting,
            capabilities: ServerCapabilities::default(),
            encoding: Encoding::default(),
            next_id: 0,
            init_id: 0,
            outgoing: out_tx,
            incoming: in_rx,
            queued: Vec::new(),
            stderr: Arc::default(),
            child: None,
        };
        client.init_id = client.take_id();
        let params = initialize_params(root);
        client.send(rpc::request(client.init_id, "initialize", params));
        client
    }

    pub fn state(&self) -> &ClientState {
        &self.state
    }

    pub fn is_ready(&self) -> bool {
        self.state == ClientState::Ready
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self.state, ClientState::Stopped(_))
    }

    /// The server's capabilities; empty until it is ready.
    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.capabilities
    }

    fn take_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    fn send(&self, msg: Value) {
        let _ = self.outgoing.send(msg);
    }

    /// Sends now, or once the handshake is done. Dropped if the server stopped.
    fn send_or_queue(&mut self, msg: Value) {
        match self.state {
            ClientState::Ready => self.send(msg),
            ClientState::Starting => self.queued.push(msg),
            ClientState::Stopped(_) => {}
        }
    }

    /// Sends a request and returns its id, to match up the response.
    pub fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.take_id();
        self.send_or_queue(rpc::request(id, method, params));
        id
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send_or_queue(rpc::notification(method, params));
    }

    pub fn cancel(&mut self, id: i64) {
        self.notify("$/cancelRequest", json!({ "id": id }));
    }

    /// Answers a request the server sent us.
    pub fn respond(&mut self, id: Value, result: Result<Value, ResponseError>) {
        if !self.is_stopped() {
            self.send(rpc::response(id, result));
        }
    }

    /// Handles everything the server sent since the last call. Handshake
    /// replies and routine server requests are dealt with here; responses,
    /// notifications and other requests are returned.
    pub fn poll(&mut self) -> Vec<Incoming> {
        let mut out = Vec::new();
        while let Ok(msg) = self.incoming.try_recv() {
            let Some(msg) = msg else {
                self.stopped();
                break;
            };
            match Incoming::parse(msg) {
                Some(Incoming::Response { id, result }) if id == self.init_id => {
                    self.initialized(result);
                }
                Some(Incoming::Request { id, method, params }) => {
                    match self.auto_reply(&method, &params) {
                        Some(reply) => self.respond(id, reply),
                        None => out.push(Incoming::Request { id, method, params }),
                    }
                }
                Some(other) => out.push(other),
                None => {}
            }
        }
        out
    }

    fn initialized(&mut self, result: Result<Value, ResponseError>) {
        match result {
            Ok(value) => {
                let result: InitializeResult = serde_json::from_value(value).unwrap_or_default();
                self.encoding = Encoding::from_kind(result.capabilities.position_encoding.as_ref());
                self.capabilities = result.capabilities;
                self.state = ClientState::Ready;
                self.send(rpc::notification("initialized", json!({})));
                for msg in std::mem::take(&mut self.queued) {
                    self.send(msg);
                }
            }
            Err(e) => {
                self.queued.clear();
                self.state = ClientState::Stopped(format!("failed to initialize: {}", e.message));
            }
        }
    }

    fn stopped(&mut self) {
        if self.is_stopped() {
            return;
        }
        let status = self
            .child
            .as_mut()
            .and_then(|c| {
                // Give the process a moment to finish exiting.
                for _ in 0..10 {
                    if let Ok(Some(status)) = c.try_wait() {
                        return Some(status);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                None
            })
            .map_or_else(|| "stopped".to_owned(), |s| format!("exited ({s})"));
        let stderr = self.stderr_tail();
        self.state = ClientState::Stopped(if stderr.is_empty() {
            status
        } else {
            format!("{status}:\n{stderr}")
        });
    }

    /// The last lines the server wrote to stderr.
    pub fn stderr_tail(&self) -> String {
        let lines = self.stderr.lock().unwrap();
        let skip = lines.len().saturating_sub(8);
        lines
            .iter()
            .skip(skip)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Replies to server requests that need nothing from the editor.
    fn auto_reply(&self, method: &str, params: &Value) -> Option<Result<Value, ResponseError>> {
        let reply = match method {
            // No settings of our own: let the server use its defaults.
            "workspace/configuration" => {
                let n = params["items"].as_array().map_or(0, Vec::len);
                Ok(Value::Array(vec![Value::Null; n]))
            }
            "workspace/workspaceFolders" => Ok(json!([workspace_folder(&self.root)])),
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest"
            | "workspace/semanticTokens/refresh"
            | "workspace/inlayHint/refresh"
            | "workspace/codeLens/refresh"
            | "workspace/diagnostic/refresh" => Ok(Value::Null),
            "workspace/applyEdit" => return None,
            _ => Err(ResponseError {
                code: rpc::METHOD_NOT_FOUND,
                message: format!("{method} is not supported"),
            }),
        };
        Some(reply)
    }
}

impl Drop for Client {
    /// Asks the server to shut down, and kills it if it hasn't after a moment.
    fn drop(&mut self) {
        if self.is_ready() {
            let id = self.take_id();
            self.send(rpc::request(id, "shutdown", Value::Null));
            self.send(rpc::notification("exit", Value::Null));
        }
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                for _ in 0..30 {
                    if let Ok(Some(_)) = child.try_wait() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

fn workspace_folder(root: &Path) -> Value {
    let name = root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    json!({ "uri": path_to_uri(root).map(|u| u.as_str().to_owned()), "name": name })
}

/// What we tell the server about ourselves. Only features the editor
/// actually uses are advertised.
fn initialize_params(root: &Path) -> Value {
    let uri = path_to_uri(root).map(|u| u.as_str().to_owned());
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "Forge IDE", "version": env!("CARGO_PKG_VERSION") },
        "rootPath": root.display().to_string(),
        "rootUri": uri,
        "workspaceFolders": [workspace_folder(root)],
        "capabilities": {
            "general": { "positionEncodings": ["utf-8", "utf-16"] },
            "window": { "workDoneProgress": true },
            "workspace": {
                "applyEdit": true,
                "workspaceEdit": { "documentChanges": true },
                "configuration": true,
                "workspaceFolders": true,
            },
            "textDocument": {
                "synchronization": { "didSave": true },
                "publishDiagnostics": { "relatedInformation": false },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "completion": {
                    "contextSupport": true,
                    "completionItem": {
                        "snippetSupport": true,
                        "documentationFormat": ["markdown", "plaintext"],
                        "deprecatedSupport": true,
                        "labelDetailsSupport": true,
                    },
                },
                "definition": { "linkSupport": true },
                "references": {},
                "rename": {},
                "formatting": {},
            },
        },
    })
}

#[cfg(test)]
pub mod tests {
    use std::io::BufReader;

    use super::*;
    use crate::lsp::catalog;

    /// A scripted server on the other end of a pair of pipes. `handle` gets
    /// every message and returns the messages to send back.
    pub fn fake_server(
        mut handle: impl FnMut(&Value) -> Vec<Value> + Send + 'static,
    ) -> (impl Read + Send + 'static, impl Write + Send + 'static) {
        let (client_reader, mut server_writer) = std::io::pipe().unwrap();
        let (server_reader, client_writer) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let mut r = BufReader::new(server_reader);
            while let Ok(Some(msg)) = rpc::read_message(&mut r) {
                for reply in handle(&msg) {
                    if rpc::write_message(&mut server_writer, &reply).is_err() {
                        return;
                    }
                }
                if msg["method"] == "exit" {
                    return;
                }
            }
        });
        (client_reader, client_writer)
    }

    /// Calls `poll` until `done` says so, or panics after a few seconds.
    pub fn poll_until(
        client: &mut Client,
        mut done: impl FnMut(&mut Client, Vec<Incoming>) -> bool,
    ) {
        for _ in 0..500 {
            let msgs = client.poll();
            if done(client, msgs) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for the fake server");
    }

    pub fn init_reply(msg: &Value, capabilities: Value) -> Vec<Value> {
        vec![rpc::response(
            msg["id"].clone(),
            Ok(json!({ "capabilities": capabilities })),
        )]
    }

    #[test]
    fn handshake_queues_messages_and_answers_server_requests() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let (r, w) = fake_server(move |msg| {
            log.lock()
                .unwrap()
                .push(msg["method"].as_str().unwrap_or("<response>").to_owned());
            match msg["method"].as_str() {
                Some("initialize") => {
                    assert_eq!(
                        msg["params"]["capabilities"]["general"]["positionEncodings"][0],
                        "utf-8"
                    );
                    init_reply(
                        msg,
                        json!({ "positionEncoding": "utf-8", "hoverProvider": true }),
                    )
                }
                Some("textDocument/hover") => vec![
                    rpc::request(77, "workspace/configuration", json!({ "items": [{}, {}] })),
                    rpc::response(msg["id"].clone(), Ok(json!({ "contents": "docs" }))),
                ],
                _ => Vec::new(),
            }
        });
        let ctx = egui::Context::default();
        let spec = catalog::find("rust-analyzer").unwrap();
        let mut client = Client::connect(spec, Path::new("/tmp"), r, w, &ctx);
        // Sent before the handshake finished: must wait for it.
        let id = client.request("textDocument/hover", json!({}));
        assert_eq!(client.state(), &ClientState::Starting);

        let mut response = None;
        poll_until(&mut client, |_, msgs| {
            for m in msgs {
                if let Incoming::Response { id: got, result } = m {
                    assert_eq!(got, id);
                    response = Some(result.unwrap());
                }
            }
            response.is_some()
        });
        assert!(client.is_ready());
        assert_eq!(client.encoding, Encoding::Utf8);
        assert!(client.capabilities().hover_provider.is_some());
        assert_eq!(response.unwrap()["contents"], "docs");

        drop(client);
        std::thread::sleep(Duration::from_millis(50));
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [
                "initialize",
                "initialized",
                "textDocument/hover",
                "<response>",
                "shutdown",
                "exit"
            ]
        );
    }

    #[test]
    fn reports_a_server_that_goes_away() {
        // A server that exits right away: its stdout is closed.
        let (r, server_stdout) = std::io::pipe().unwrap();
        let (_server_stdin, w) = std::io::pipe().unwrap();
        drop(server_stdout);
        let ctx = egui::Context::default();
        let spec = catalog::find("rust-analyzer").unwrap();
        let mut client = Client::connect(spec, Path::new("/tmp"), r, w, &ctx);
        poll_until(&mut client, |c, _| c.is_stopped());
    }
}

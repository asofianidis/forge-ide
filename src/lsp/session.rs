//! Language servers for the open files: starts the right server for each file
//! and workspace, keeps it in sync with the buffers, and turns its replies into
//! [`LspEvent`]s for the editor.
//!
//! Documents are always synced in full (every change sends the whole text).
//! That is valid for every server whatever sync kind it prefers, and keeps the
//! client simple; requests flush pending changes first so they never see
//! stale text.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use lsp_types::{
    CompletionItem, CompletionResponse, Diagnostic, DiagnosticSeverity, DocumentChangeOperation,
    DocumentChanges, GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability,
    MarkedString, MarkupKind, NumberOrString, OneOf, ProgressParams, ProgressParamsValue,
    PublishDiagnosticsParams, ServerCapabilities, ShowMessageParams, TextDocumentSyncCapability,
    TextDocumentSyncSaveOptions, TextEdit, Uri, WorkDoneProgress, WorkspaceEdit,
};
use serde_json::{Value, json};

use super::catalog::ServerSpec;
use super::client::{Client, ClientState};
use super::rpc::{self, Incoming, ResponseError};
use super::text::{Encoding, LineIndex, language_id, path_to_uri, uri_to_path};

/// A server instance: which server, for which workspace root.
pub type ServerKey = (&'static str, PathBuf);

/// Starts a server; replaced in tests to talk to a fake one.
pub type Spawner =
    Box<dyn FnMut(&'static ServerSpec, &Path, &Path, &egui::Context) -> io::Result<Client>>;

/// A position-based feature, for checking that the server offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Hover,
    Definition,
    References,
    Completion,
    Rename,
    Formatting,
}

impl Feature {
    fn describe(self) -> &'static str {
        match self {
            Feature::Hover => "hover",
            Feature::Definition => "go to definition",
            Feature::References => "find references",
            Feature::Completion => "completion",
            Feature::Rename => "rename",
            Feature::Formatting => "formatting",
        }
    }

    fn supported_by(self, caps: &ServerCapabilities) -> bool {
        fn enabled<T>(o: &Option<OneOf<bool, T>>) -> bool {
            matches!(o, Some(OneOf::Left(true) | OneOf::Right(_)))
        }
        match self {
            Feature::Hover => matches!(
                caps.hover_provider,
                Some(HoverProviderCapability::Simple(true) | HoverProviderCapability::Options(_))
            ),
            Feature::Definition => enabled(&caps.definition_provider),
            Feature::References => enabled(&caps.references_provider),
            Feature::Completion => caps.completion_provider.is_some(),
            Feature::Rename => enabled(&caps.rename_provider),
            Feature::Formatting => enabled(&caps.document_formatting_provider),
        }
    }
}

/// A place in a file, as the server reported it.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub path: PathBuf,
    pub range: lsp_types::Range,
    pub encoding: Encoding,
}

impl Location {
    pub fn chars(&self, text: &str) -> Range<usize> {
        LineIndex::new(text).range_of(&self.range, self.encoding)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationKind {
    Definition,
    References,
}

/// Something the editor should act on.
#[derive(Debug)]
pub enum LspEvent {
    /// Hover docs for the char `at`, or `None` if the server had nothing.
    Hover {
        path: PathBuf,
        at: usize,
        markdown: Option<String>,
        /// Chars the docs are about, in the text as the server saw it.
        range: Option<Range<usize>>,
    },
    Locations {
        kind: LocationKind,
        locations: Vec<Location>,
    },
    Completion {
        path: PathBuf,
        at: usize,
        items: Vec<CompletionItem>,
        encoding: Encoding,
        /// More typing may produce different items, so ask again.
        incomplete: bool,
    },
    /// Edits across files: a rename, or a server's `workspace/applyEdit`. A
    /// `reply` must be answered with [`Lsp::reply_apply_edit`].
    WorkspaceEdit {
        label: &'static str,
        edit: WorkspaceEdit,
        encoding: Encoding,
        reply: Option<(ServerKey, Value)>,
    },
    /// Formatting edits (possibly none) for `path`. When `then_save`, the
    /// file is saved after applying them, also if formatting failed.
    Format {
        path: PathBuf,
        edits: Vec<TextEdit>,
        encoding: Encoding,
        then_save: bool,
    },
    /// Something worth showing in the status bar.
    Message(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    fn of(d: &Diagnostic) -> Self {
        match d.severity {
            Some(DiagnosticSeverity::WARNING) => Severity::Warning,
            Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
            Some(DiagnosticSeverity::HINT) => Severity::Hint,
            // Unspecified counts as an error, as in other editors.
            _ => Severity::Error,
        }
    }
}

/// A diagnostic placed in the current text of a buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    /// Where it came from, e.g. `rustc(E0308)`.
    pub source: Option<String>,
}

/// A diagnostic anywhere in the workspace, for the problems list.
pub struct WorkspaceProblem {
    pub path: PathBuf,
    pub location: Location,
    pub severity: Severity,
    pub message: String,
}

/// What a server is doing, for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerActivity {
    Starting,
    Ready,
    /// Work in progress, e.g. "Indexing 40%".
    Busy(String),
    Stopped(String),
}

struct OpenDoc {
    key: ServerKey,
    uri: Uri,
    version: i32,
    /// The text as last sent to the server.
    text: String,
}

enum Pending {
    Hover { path: PathBuf, at: usize },
    Locations(LocationKind),
    Completion { path: PathBuf, at: usize },
    Rename,
    Format { path: PathBuf, then_save: bool },
}

#[derive(Default)]
struct Progress {
    title: String,
    message: Option<String>,
    percentage: Option<u32>,
}

pub struct Lsp {
    ctx: egui::Context,
    spawn: Spawner,
    clients: HashMap<ServerKey, Client>,
    /// Servers that could not be started, with the error. Not retried until
    /// [`Lsp::restart`].
    failed: HashMap<ServerKey, String>,
    docs: HashMap<PathBuf, OpenDoc>,
    diagnostics: HashMap<PathBuf, (ServerKey, Vec<Diagnostic>)>,
    pending: HashMap<(ServerKey, i64), Pending>,
    /// The newest hover / completion request; older ones are cancelled.
    latest: HashMap<&'static str, (ServerKey, i64)>,
    progress: BTreeMap<(ServerKey, String), Progress>,
    /// Stopped servers already reported.
    reported: HashSet<ServerKey>,
    events: Vec<LspEvent>,
}

/// The `FormattingOptions` sent with a formatting request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub trim_trailing_whitespace: bool,
    pub insert_final_newline: bool,
}

impl FormatOptions {
    /// Options for indenting with `indent` (a tab, or spaces) where a tab is
    /// `tab_width` columns, trimming whitespace and ending with a newline.
    pub fn from_indent(indent: &str, tab_width: usize) -> Self {
        Self {
            tab_size: if indent == "\t" {
                tab_width
            } else {
                indent.len()
            }
            .max(1),
            insert_spaces: indent != "\t",
            trim_trailing_whitespace: true,
            insert_final_newline: true,
        }
    }

    /// Options following the document's indentation and `.editorconfig`.
    pub fn for_document(doc: &crate::editor::Document) -> Self {
        let mut options = Self::from_indent(&doc.indent_unit(), doc.tab_width());
        let ec = &doc.editorconfig;
        if let Some(trim) = ec.trim_trailing_whitespace {
            options.trim_trailing_whitespace = trim;
        }
        if let Some(final_newline) = ec.insert_final_newline {
            options.insert_final_newline = final_newline;
        }
        options
    }
}

impl Lsp {
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_spawner(ctx, Box::new(Client::spawn))
    }

    pub fn with_spawner(ctx: egui::Context, spawn: Spawner) -> Self {
        Self {
            ctx,
            spawn,
            clients: HashMap::new(),
            failed: HashMap::new(),
            docs: HashMap::new(),
            diagnostics: HashMap::new(),
            pending: HashMap::new(),
            latest: HashMap::new(),
            progress: BTreeMap::new(),
            reported: HashSet::new(),
            events: Vec::new(),
        }
    }

    /// Stops every server and forgets all state. Servers start again for the
    /// open files on the next [`Lsp::sync`].
    pub fn restart(&mut self) {
        // Formatting that was going to save the file: save it anyway.
        for (_, pending) in self.pending.drain() {
            if let Pending::Format {
                path,
                then_save: true,
            } = pending
            {
                self.events.push(LspEvent::Format {
                    path,
                    edits: Vec::new(),
                    encoding: Encoding::default(),
                    then_save: true,
                });
            }
        }
        self.clients.clear();
        self.failed.clear();
        self.docs.clear();
        self.diagnostics.clear();
        self.latest.clear();
        self.progress.clear();
        self.reported.clear();
    }

    /// Brings the servers in line with the open buffers: starts servers for
    /// new files, sends edits, and closes files that are no longer open.
    /// `find_server` picks the installed server (and its executable) for a
    /// file name. Files outside `root` get their own folder as the root.
    pub fn sync(
        &mut self,
        docs: &[(&Path, &str)],
        root: Option<&Path>,
        find_server: impl Fn(&str) -> Option<(&'static ServerSpec, PathBuf)>,
    ) {
        let open: HashSet<&Path> = docs.iter().map(|(p, _)| *p).collect();
        let closed: Vec<PathBuf> = self
            .docs
            .keys()
            .filter(|p| !open.contains(p.as_path()))
            .cloned()
            .collect();
        for path in closed {
            let doc = self.docs.remove(&path).expect("listed above");
            if let Some(client) = self.clients.get_mut(&doc.key) {
                client.notify(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": doc.uri } }),
                );
            }
        }
        for &(path, text) in docs {
            if self.docs.contains_key(path) {
                self.flush(path, text);
            } else {
                self.open(path, text, root, &find_server);
            }
        }
    }

    fn open(
        &mut self,
        path: &Path,
        text: &str,
        root: Option<&Path>,
        find_server: &impl Fn(&str) -> Option<(&'static ServerSpec, PathBuf)>,
    ) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let Some((spec, program)) = find_server(name) else {
            return;
        };
        let root = match root {
            Some(r) if path.starts_with(r) => r.to_path_buf(),
            _ => match path.parent() {
                Some(p) => p.to_path_buf(),
                None => return,
            },
        };
        let Some(uri) = path_to_uri(path) else {
            return;
        };
        let key = (spec.id, root);
        if self.failed.contains_key(&key) {
            return;
        }
        if !self.clients.contains_key(&key) {
            match (self.spawn)(spec, &program, &key.1, &self.ctx) {
                Ok(client) => {
                    self.clients.insert(key.clone(), client);
                }
                Err(e) => {
                    let msg = format!("Couldn't start {}: {e}", spec.name);
                    self.events.push(LspEvent::Message(msg.clone()));
                    self.failed.insert(key, msg);
                    return;
                }
            }
        }
        let client = self.clients.get_mut(&key).expect("inserted above");
        client.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id(path),
                    "version": 0,
                    "text": text,
                }
            }),
        );
        self.docs.insert(
            path.to_path_buf(),
            OpenDoc {
                key,
                uri,
                version: 0,
                text: text.to_owned(),
            },
        );
    }

    /// Sends the buffer's text if it changed since it was last sent.
    fn flush(&mut self, path: &Path, text: &str) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        if doc.text == text {
            return;
        }
        doc.version += 1;
        doc.text.clear();
        doc.text.push_str(text);
        if let Some(client) = self.clients.get_mut(&doc.key) {
            client.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": doc.uri, "version": doc.version },
                    "contentChanges": [{ "text": text }],
                }),
            );
        }
    }

    /// Tells the server a file was saved (many servers re-check on save).
    pub fn did_save(&mut self, path: &Path, text: &str) {
        self.flush(path, text);
        let Some(doc) = self.docs.get(path) else {
            return;
        };
        let Some(client) = self.clients.get_mut(&doc.key) else {
            return;
        };
        let include_text = matches!(
            &client.capabilities().text_document_sync,
            Some(TextDocumentSyncCapability::Options(o))
                if matches!(&o.save, Some(TextDocumentSyncSaveOptions::SaveOptions(s)) if s.include_text == Some(true))
        );
        let mut params = json!({ "textDocument": { "uri": doc.uri } });
        if include_text {
            params["text"] = json!(text);
        }
        client.notify("textDocument/didSave", params);
    }

    /// The server for `path` if it is ready and offers `feature`; otherwise a
    /// message saying why not.
    fn prepare(
        &mut self,
        path: &Path,
        text: &str,
        feature: Feature,
    ) -> Result<(ServerKey, Uri, Encoding), String> {
        self.flush(path, text);
        let file = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let Some(doc) = self.docs.get(path) else {
            return Err(format!("No language server is running for {file}"));
        };
        let Some(client) = self.clients.get(&doc.key) else {
            return Err(format!("No language server is running for {file}"));
        };
        let name = client.server.name;
        match client.state() {
            ClientState::Starting => Err(format!("{name} is still starting")),
            ClientState::Stopped(_) => Err(format!(
                "{name} has stopped (Tools → Restart Language Servers)"
            )),
            ClientState::Ready if !feature.supported_by(client.capabilities()) => {
                Err(format!("{name} doesn't support {}", feature.describe()))
            }
            ClientState::Ready => Ok((doc.key.clone(), doc.uri.clone(), client.encoding)),
        }
    }

    /// Whether `path` has a ready server that offers `feature`.
    pub fn supports(&self, path: &Path, feature: Feature) -> bool {
        self.docs
            .get(path)
            .and_then(|d| self.clients.get(&d.key))
            .is_some_and(|c| c.is_ready() && feature.supported_by(c.capabilities()))
    }

    fn send_request(
        &mut self,
        key: ServerKey,
        method: &'static str,
        params: Value,
        pending: Pending,
        supersede: bool,
    ) {
        let Some(client) = self.clients.get_mut(&key) else {
            return;
        };
        let id = client.request(method, params);
        if supersede
            && let Some((old_key, old_id)) = self.latest.insert(method, (key.clone(), id))
            && self.pending.remove(&(old_key.clone(), old_id)).is_some()
            && let Some(old) = self.clients.get_mut(&old_key)
        {
            old.cancel(old_id);
        }
        self.pending.insert((key, id), pending);
    }

    fn position_params(uri: &Uri, text: &str, at: usize, enc: Encoding) -> Value {
        let position = LineIndex::new(text).position_of(at, enc);
        json!({ "textDocument": { "uri": uri }, "position": position })
    }

    pub fn hover(&mut self, path: &Path, text: &str, at: usize) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::Hover)?;
        let params = Self::position_params(&uri, text, at, enc);
        let pending = Pending::Hover {
            path: path.to_path_buf(),
            at,
        };
        self.send_request(key, "textDocument/hover", params, pending, true);
        Ok(())
    }

    pub fn definition(&mut self, path: &Path, text: &str, at: usize) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::Definition)?;
        let params = Self::position_params(&uri, text, at, enc);
        let pending = Pending::Locations(LocationKind::Definition);
        self.send_request(key, "textDocument/definition", params, pending, true);
        Ok(())
    }

    pub fn references(&mut self, path: &Path, text: &str, at: usize) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::References)?;
        let mut params = Self::position_params(&uri, text, at, enc);
        params["context"] = json!({ "includeDeclaration": true });
        let pending = Pending::Locations(LocationKind::References);
        self.send_request(key, "textDocument/references", params, pending, true);
        Ok(())
    }

    /// Asks for completions at `at`. `trigger` is the character just typed
    /// if it is one of the server's trigger characters.
    pub fn completion(
        &mut self,
        path: &Path,
        text: &str,
        at: usize,
        trigger: Option<char>,
    ) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::Completion)?;
        let mut params = Self::position_params(&uri, text, at, enc);
        params["context"] = match trigger {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        let pending = Pending::Completion {
            path: path.to_path_buf(),
            at,
        };
        self.send_request(key, "textDocument/completion", params, pending, true);
        Ok(())
    }

    /// Characters that should open completion when typed, e.g. `.` and `::`.
    pub fn completion_triggers(&self, path: &Path) -> Vec<char> {
        self.docs
            .get(path)
            .and_then(|d| self.clients.get(&d.key))
            .and_then(|c| c.capabilities().completion_provider.as_ref())
            .and_then(|p| p.trigger_characters.as_ref())
            .map(|t| t.iter().filter_map(|s| s.chars().last()).collect())
            .unwrap_or_default()
    }

    pub fn rename(
        &mut self,
        path: &Path,
        text: &str,
        at: usize,
        new_name: &str,
    ) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::Rename)?;
        let mut params = Self::position_params(&uri, text, at, enc);
        params["newName"] = json!(new_name);
        self.send_request(key, "textDocument/rename", params, Pending::Rename, false);
        Ok(())
    }

    /// Asks for formatting edits for the whole file.
    pub fn format(
        &mut self,
        path: &Path,
        text: &str,
        options: &FormatOptions,
        then_save: bool,
    ) -> Result<(), String> {
        let (key, uri, _) = self.prepare(path, text, Feature::Formatting)?;
        let params = json!({
            "textDocument": { "uri": uri },
            "options": {
                "tabSize": options.tab_size,
                "insertSpaces": options.insert_spaces,
                "trimTrailingWhitespace": options.trim_trailing_whitespace,
                "insertFinalNewline": options.insert_final_newline,
            },
        });
        let pending = Pending::Format {
            path: path.to_path_buf(),
            then_save,
        };
        self.send_request(key, "textDocument/formatting", params, pending, false);
        Ok(())
    }

    /// Answers a server's `workspace/applyEdit` request.
    pub fn reply_apply_edit(&mut self, key: &ServerKey, id: Value, applied: bool) {
        if let Some(client) = self.clients.get_mut(key) {
            client.respond(id, Ok(json!({ "applied": applied })));
        }
    }

    /// Processes everything the servers sent. Call once per frame.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let keys: Vec<ServerKey> = self.clients.keys().cloned().collect();
        for key in keys {
            let Some(client) = self.clients.get_mut(&key) else {
                continue;
            };
            for msg in client.poll() {
                self.handle(&key, msg);
            }
            let client = &self.clients[&key];
            if let ClientState::Stopped(reason) = client.state()
                && self.reported.insert(key.clone())
            {
                let first_line = reason.lines().next().unwrap_or_default();
                self.events.push(LspEvent::Message(format!(
                    "{} {first_line}",
                    client.server.name
                )));
                self.forget_server(&key);
            }
        }
        std::mem::take(&mut self.events)
    }

    /// Drops what a stopped server left behind.
    fn forget_server(&mut self, key: &ServerKey) {
        self.diagnostics.retain(|_, (k, _)| k != key);
        self.progress.retain(|(k, _), _| k != key);
        let ids: Vec<_> = self
            .pending
            .keys()
            .filter(|(k, _)| k == key)
            .cloned()
            .collect();
        for id in ids {
            if let Some(Pending::Format {
                path,
                then_save: true,
            }) = self.pending.remove(&id)
            {
                self.events.push(LspEvent::Format {
                    path,
                    edits: Vec::new(),
                    encoding: Encoding::default(),
                    then_save: true,
                });
            }
        }
    }

    fn encoding(&self, key: &ServerKey) -> Encoding {
        self.clients
            .get(key)
            .map(|c| c.encoding)
            .unwrap_or_default()
    }

    fn server_name(&self, key: &ServerKey) -> &'static str {
        self.clients.get(key).map_or(key.0, |c| c.server.name)
    }

    fn handle(&mut self, key: &ServerKey, msg: Incoming) {
        match msg {
            Incoming::Response { id, result } => {
                let Some(pending) = self.pending.remove(&(key.clone(), id)) else {
                    return;
                };
                self.latest.retain(|_, v| !(v.0 == *key && v.1 == id));
                match result {
                    Ok(value) => self.on_result(key, pending, value),
                    Err(e) => self.on_error(key, pending, &e),
                }
            }
            Incoming::Notification { method, params } => self.on_notification(key, &method, params),
            Incoming::Request { id, method, params } => {
                if method == "workspace/applyEdit"
                    && let Some(edit) = params.get("edit").cloned()
                    && let Ok(edit) = serde_json::from_value::<WorkspaceEdit>(edit)
                {
                    self.events.push(LspEvent::WorkspaceEdit {
                        label: "Edit",
                        edit,
                        encoding: self.encoding(key),
                        reply: Some((key.clone(), id)),
                    });
                } else if let Some(client) = self.clients.get_mut(key) {
                    client.respond(
                        id,
                        Err(ResponseError {
                            code: rpc::METHOD_NOT_FOUND,
                            message: format!("{method} is not supported"),
                        }),
                    );
                }
            }
        }
    }

    fn on_error(&mut self, key: &ServerKey, pending: Pending, e: &ResponseError) {
        let quiet = matches!(e.code, rpc::REQUEST_CANCELLED | rpc::CONTENT_MODIFIED);
        let what = match pending {
            Pending::Hover { .. } | Pending::Completion { .. } => return,
            Pending::Format { path, then_save } => {
                self.events.push(LspEvent::Format {
                    path,
                    edits: Vec::new(),
                    encoding: Encoding::default(),
                    then_save,
                });
                "Formatting"
            }
            Pending::Locations(LocationKind::Definition) => "Go to definition",
            Pending::Locations(LocationKind::References) => "Find references",
            Pending::Rename => "Rename",
        };
        if !quiet {
            let name = self.server_name(key);
            self.events.push(LspEvent::Message(format!(
                "{what} failed ({name}): {}",
                e.message
            )));
        }
    }

    fn on_result(&mut self, key: &ServerKey, pending: Pending, value: Value) {
        let encoding = self.encoding(key);
        match pending {
            Pending::Hover { path, at } => {
                let hover: Option<Hover> = serde_json::from_value(value).ok().flatten();
                let range = hover.as_ref().and_then(|h| h.range).and_then(|r| {
                    let doc = self.docs.get(&path)?;
                    Some(LineIndex::new(&doc.text).range_of(&r, encoding))
                });
                let markdown = hover
                    .map(|h| hover_markdown(h.contents))
                    .filter(|m| !m.trim().is_empty());
                self.events.push(LspEvent::Hover {
                    path,
                    at,
                    markdown,
                    range,
                });
            }
            Pending::Locations(kind) => {
                let locations = match kind {
                    LocationKind::Definition => {
                        serde_json::from_value::<Option<GotoDefinitionResponse>>(value)
                            .ok()
                            .flatten()
                            .map(|r| definition_locations(r, encoding))
                            .unwrap_or_default()
                    }
                    LocationKind::References => {
                        serde_json::from_value::<Option<Vec<lsp_types::Location>>>(value)
                            .ok()
                            .flatten()
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|l| location(&l.uri, l.range, encoding))
                            .collect()
                    }
                };
                self.events.push(LspEvent::Locations { kind, locations });
            }
            Pending::Completion { path, at } => {
                let (items, incomplete) =
                    match serde_json::from_value::<Option<CompletionResponse>>(value) {
                        Ok(Some(CompletionResponse::Array(items))) => (items, false),
                        Ok(Some(CompletionResponse::List(list))) => {
                            (list.items, list.is_incomplete)
                        }
                        _ => (Vec::new(), false),
                    };
                self.events.push(LspEvent::Completion {
                    path,
                    at,
                    items,
                    encoding,
                    incomplete,
                });
            }
            Pending::Rename => match serde_json::from_value::<Option<WorkspaceEdit>>(value) {
                Ok(Some(edit)) => self.events.push(LspEvent::WorkspaceEdit {
                    label: "Rename",
                    edit,
                    encoding,
                    reply: None,
                }),
                _ => self
                    .events
                    .push(LspEvent::Message("Nothing to rename here".to_owned())),
            },
            Pending::Format { path, then_save } => {
                let edits = serde_json::from_value::<Option<Vec<TextEdit>>>(value)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                self.events.push(LspEvent::Format {
                    path,
                    edits,
                    encoding,
                    then_save,
                });
            }
        }
    }

    fn on_notification(&mut self, key: &ServerKey, method: &str, params: Value) {
        match method {
            "textDocument/publishDiagnostics" => {
                let Ok(p) = serde_json::from_value::<PublishDiagnosticsParams>(params) else {
                    return;
                };
                let Some(path) = uri_to_path(&p.uri) else {
                    return;
                };
                if p.diagnostics.is_empty() {
                    self.diagnostics.remove(&path);
                } else {
                    self.diagnostics.insert(path, (key.clone(), p.diagnostics));
                }
            }
            "window/showMessage" => {
                if let Ok(p) = serde_json::from_value::<ShowMessageParams>(params)
                    && matches!(
                        p.typ,
                        lsp_types::MessageType::ERROR | lsp_types::MessageType::WARNING
                    )
                {
                    let name = self.server_name(key);
                    self.events
                        .push(LspEvent::Message(format!("{name}: {}", p.message)));
                }
            }
            "$/progress" => {
                let Ok(p) = serde_json::from_value::<ProgressParams>(params) else {
                    return;
                };
                let token = match p.token {
                    NumberOrString::Number(n) => n.to_string(),
                    NumberOrString::String(s) => s,
                };
                let id = (key.clone(), token);
                let ProgressParamsValue::WorkDone(work) = p.value;
                match work {
                    WorkDoneProgress::Begin(b) => {
                        self.progress.insert(
                            id,
                            Progress {
                                title: b.title,
                                message: b.message,
                                percentage: b.percentage,
                            },
                        );
                    }
                    WorkDoneProgress::Report(r) => {
                        if let Some(p) = self.progress.get_mut(&id) {
                            if r.message.is_some() {
                                p.message = r.message;
                            }
                            if r.percentage.is_some() {
                                p.percentage = r.percentage;
                            }
                        }
                    }
                    WorkDoneProgress::End(_) => {
                        self.progress.remove(&id);
                    }
                }
            }
            _ => {}
        }
    }

    /// The diagnostics for `path`, placed in `text`, most severe first.
    pub fn problems(&self, path: &Path, text: &str) -> Vec<Problem> {
        let Some((key, diagnostics)) = self.diagnostics.get(path) else {
            return Vec::new();
        };
        let enc = self.encoding(key);
        let index = LineIndex::new(text);
        let mut problems: Vec<Problem> = diagnostics
            .iter()
            .map(|d| Problem {
                range: index.range_of(&d.range, enc),
                severity: Severity::of(d),
                message: d.message.clone(),
                source: diagnostic_source(d),
            })
            .collect();
        problems.sort_by_key(|p| (p.severity, p.range.start));
        problems
    }

    /// Every diagnostic in the workspace, by file and position.
    pub fn workspace_problems(&self) -> Vec<WorkspaceProblem> {
        let mut all: Vec<WorkspaceProblem> = self
            .diagnostics
            .iter()
            .flat_map(|(path, (key, diagnostics))| {
                let encoding = self.encoding(key);
                diagnostics.iter().map(move |d| WorkspaceProblem {
                    path: path.clone(),
                    location: Location {
                        path: path.clone(),
                        range: d.range,
                        encoding,
                    },
                    severity: Severity::of(d),
                    message: d.message.clone(),
                })
            })
            .collect();
        all.sort_by(|a, b| {
            (
                a.severity,
                &a.path,
                a.location.range.start.line,
                a.location.range.start.character,
            )
                .cmp(&(
                    b.severity,
                    &b.path,
                    b.location.range.start.line,
                    b.location.range.start.character,
                ))
        });
        all
    }

    /// Errors and warnings across the workspace.
    pub fn counts(&self) -> (usize, usize) {
        let mut counts = (0, 0);
        for (_, diagnostics) in self.diagnostics.values() {
            for d in diagnostics {
                match Severity::of(d) {
                    Severity::Error => counts.0 += 1,
                    Severity::Warning => counts.1 += 1,
                    _ => {}
                }
            }
        }
        counts
    }

    /// The server for `path` and what it is doing, if one is running.
    pub fn activity(&self, path: &Path) -> Option<(&'static str, ServerActivity)> {
        let doc = self.docs.get(path)?;
        let client = self.clients.get(&doc.key)?;
        let activity = match client.state() {
            ClientState::Starting => ServerActivity::Starting,
            ClientState::Stopped(reason) => ServerActivity::Stopped(reason.clone()),
            ClientState::Ready => self
                .progress
                .iter()
                .find(|((k, _), _)| *k == doc.key)
                .map_or(ServerActivity::Ready, |(_, p)| {
                    let mut text = p.title.clone();
                    if let Some(m) = &p.message {
                        text = format!("{text}: {m}");
                    }
                    if let Some(pct) = p.percentage {
                        text = format!("{text} ({pct}%)");
                    }
                    ServerActivity::Busy(text)
                }),
        };
        Some((client.server.name, activity))
    }
}

fn location(uri: &Uri, range: lsp_types::Range, encoding: Encoding) -> Option<Location> {
    Some(Location {
        path: uri_to_path(uri)?,
        range,
        encoding,
    })
}

fn definition_locations(r: GotoDefinitionResponse, encoding: Encoding) -> Vec<Location> {
    match r {
        GotoDefinitionResponse::Scalar(l) => {
            location(&l.uri, l.range, encoding).into_iter().collect()
        }
        GotoDefinitionResponse::Array(ls) => ls
            .into_iter()
            .filter_map(|l| location(&l.uri, l.range, encoding))
            .collect(),
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .filter_map(|l| location(&l.target_uri, l.target_selection_range, encoding))
            .collect(),
    }
}

fn diagnostic_source(d: &Diagnostic) -> Option<String> {
    let code = d.code.as_ref().map(|c| match c {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s.clone(),
    });
    match (&d.source, code) {
        (Some(s), Some(c)) => Some(format!("{s}({c})")),
        (Some(s), None) => Some(s.clone()),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

/// Hover contents as Markdown.
fn hover_markdown(contents: HoverContents) -> String {
    fn marked(m: MarkedString) -> String {
        match m {
            MarkedString::String(s) => s,
            MarkedString::LanguageString(l) => format!("```{}\n{}\n```", l.language, l.value),
        }
    }
    match contents {
        HoverContents::Scalar(m) => marked(m),
        HoverContents::Array(ms) => ms
            .into_iter()
            .map(marked)
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
        HoverContents::Markup(m) if m.kind == MarkupKind::PlainText => {
            format!("```text\n{}\n```", m.value)
        }
        HoverContents::Markup(m) => m.value,
    }
}

/// The files a workspace edit changes, with their edits. Also returns how
/// many file operations (create / rename / delete) it asked for, which are
/// not supported.
pub fn workspace_edit_files(edit: WorkspaceEdit) -> (Vec<(PathBuf, Vec<TextEdit>)>, usize) {
    let mut files: Vec<(PathBuf, Vec<TextEdit>)> = Vec::new();
    let mut skipped = 0;
    let mut add = |uri: &Uri, edits: Vec<TextEdit>| {
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        match files.iter_mut().find(|(p, _)| *p == path) {
            Some((_, existing)) => existing.extend(edits),
            None => files.push((path, edits)),
        }
    };
    let text_edits = |edits: Vec<OneOf<TextEdit, lsp_types::AnnotatedTextEdit>>| {
        edits
            .into_iter()
            .map(|e| match e {
                OneOf::Left(e) => e,
                OneOf::Right(a) => a.text_edit,
            })
            .collect::<Vec<_>>()
    };
    match edit.document_changes {
        Some(DocumentChanges::Edits(edits)) => {
            for e in edits {
                add(&e.text_document.uri, text_edits(e.edits));
            }
        }
        Some(DocumentChanges::Operations(ops)) => {
            for op in ops {
                match op {
                    DocumentChangeOperation::Edit(e) => {
                        add(&e.text_document.uri, text_edits(e.edits));
                    }
                    DocumentChangeOperation::Op(_) => skipped += 1,
                }
            }
        }
        None => {
            for (uri, edits) in edit.changes.unwrap_or_default() {
                add(&uri, edits);
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    (files, skipped)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::lsp::catalog;
    use crate::lsp::client::tests::{fake_server, init_reply};

    /// A manager whose servers are fake: they record what they receive and
    /// answer hover, definition and formatting.
    fn fake_lsp(log: Arc<Mutex<Vec<Value>>>) -> Lsp {
        let spawner: Spawner = Box::new(move |spec, _program, root, ctx| {
            let log = Arc::clone(&log);
            let (r, w) = fake_server(move |msg| {
                log.lock().unwrap().push(msg.clone());
                let id = msg["id"].clone();
                match msg["method"].as_str() {
                    Some("initialize") => init_reply(
                        msg,
                        json!({
                            "positionEncoding": "utf-16",
                            "hoverProvider": true,
                            "definitionProvider": true,
                            "documentFormattingProvider": true,
                            "completionProvider": { "triggerCharacters": ["."] },
                        }),
                    ),
                    Some("textDocument/didOpen") => {
                        let uri = msg["params"]["textDocument"]["uri"].clone();
                        vec![rpc::notification(
                            "textDocument/publishDiagnostics",
                            json!({
                                "uri": uri,
                                "diagnostics": [
                                    { "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 3 } },
                                      "severity": 1, "message": "bad", "source": "fake", "code": "E1" },
                                    { "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 4 } },
                                      "severity": 2, "message": "meh" },
                                ],
                            }),
                        )]
                    }
                    Some("textDocument/hover") => vec![rpc::response(
                        id,
                        Ok(json!({
                            "contents": { "kind": "markdown", "value": "**doc**" },
                            "range": { "start": msg["params"]["position"], "end": { "line": 1, "character": 3 } },
                        })),
                    )],
                    Some("textDocument/definition") => {
                        let uri = msg["params"]["textDocument"]["uri"].clone();
                        vec![rpc::response(
                            id,
                            Ok(json!([{ "targetUri": uri,
                                "targetRange": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                                "targetSelectionRange": { "start": { "line": 0, "character": 1 }, "end": { "line": 0, "character": 2 } } }])),
                        )]
                    }
                    Some("textDocument/formatting") => {
                        vec![rpc::response(
                            id,
                            Err(ResponseError {
                                code: -32603,
                                message: "boom".into(),
                            }),
                        )]
                    }
                    _ => Vec::new(),
                }
            });
            Ok(Client::connect(spec, root, r, w, ctx))
        });
        Lsp::with_spawner(egui::Context::default(), spawner)
    }

    fn find_rust(name: &str) -> Option<(&'static ServerSpec, PathBuf)> {
        name.ends_with(".rs")
            .then(|| (catalog::find("rust-analyzer").unwrap(), PathBuf::from("ra")))
    }

    fn poll_until(lsp: &mut Lsp, mut done: impl FnMut(&mut Lsp, &[LspEvent]) -> bool) {
        for _ in 0..500 {
            let events = lsp.poll();
            if done(lsp, &events) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn syncs_documents_and_answers_requests() {
        let root = std::env::temp_dir().join("forge-lsp-test");
        let file = root.join("main.rs");
        let other = root.join("notes.txt");
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut lsp = fake_lsp(Arc::clone(&log));

        let text = "fn main() {}\nlet\n";
        lsp.sync(&[(&file, text), (&other, "x")], Some(&root), find_rust);
        assert_eq!(lsp.activity(&file).unwrap().1, ServerActivity::Starting);
        assert!(lsp.activity(&other).is_none(), "no server for .txt files");
        assert!(lsp.hover(&file, text, 0).is_err(), "not ready yet");

        poll_until(&mut lsp, |l, _| !l.problems(&file, text).is_empty());
        let problems = lsp.problems(&file, text);
        assert_eq!(problems[0].severity, Severity::Error);
        assert_eq!(problems[0].range, 13..16);
        assert_eq!(problems[0].source.as_deref(), Some("fake(E1)"));
        assert_eq!(problems[1].range, 2..4);
        assert_eq!(lsp.counts(), (1, 1));
        assert_eq!(lsp.completion_triggers(&file), vec!['.']);

        // An edit is flushed before the request that depends on it.
        let edited = "fn main() {}\nlet x\n";
        lsp.hover(&file, edited, 13).unwrap();
        let mut hover = None;
        poll_until(&mut lsp, |_, events| {
            for e in events {
                if let LspEvent::Hover {
                    markdown,
                    range,
                    at,
                    ..
                } = e
                {
                    hover = Some((markdown.clone(), range.clone(), *at));
                }
            }
            hover.is_some()
        });
        assert_eq!(hover, Some((Some("**doc**".into()), Some(13..16), 13)));

        lsp.definition(&file, edited, 13).unwrap();
        let mut target = None;
        poll_until(&mut lsp, |_, events| {
            for e in events {
                if let LspEvent::Locations { locations, .. } = e {
                    target = locations.first().map(|l| (l.path.clone(), l.chars(edited)));
                }
            }
            target.is_some()
        });
        assert_eq!(target, Some((file.clone(), 1..2)));

        // Formatting that fails still lets the save go ahead.
        lsp.format(&file, edited, &FormatOptions::from_indent("    ", 4), true)
            .unwrap();
        let mut saved = false;
        poll_until(&mut lsp, |_, events| {
            for e in events {
                if let LspEvent::Format {
                    then_save, edits, ..
                } = e
                {
                    saved = *then_save && edits.is_empty();
                }
            }
            saved
        });

        lsp.did_save(&file, edited);
        lsp.sync(&[], Some(&root), find_rust);
        std::thread::sleep(Duration::from_millis(50));

        let methods: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| m["method"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(
            methods,
            [
                "initialize",
                "initialized",
                "textDocument/didOpen",
                "textDocument/didChange",
                "textDocument/hover",
                "textDocument/definition",
                "textDocument/formatting",
                "textDocument/didSave",
                "textDocument/didClose",
            ]
        );
        let log = log.lock().unwrap();
        let change = log
            .iter()
            .find(|m| m["method"] == "textDocument/didChange")
            .unwrap();
        assert_eq!(change["params"]["textDocument"]["version"], 1);
        assert_eq!(change["params"]["contentChanges"][0]["text"], edited);
        let open = log
            .iter()
            .find(|m| m["method"] == "textDocument/didOpen")
            .unwrap();
        assert_eq!(open["params"]["textDocument"]["languageId"], "rust");
        assert!(
            log.iter().find(|m| m["method"] == "initialize").unwrap()["params"]["rootUri"]
                .as_str()
                .unwrap()
                .ends_with("forge-lsp-test")
        );
    }

    #[test]
    fn splits_workspace_edits_by_file() {
        let a = std::env::temp_dir().join("a.rs");
        let b = std::env::temp_dir().join("b.rs");
        let edit = |line| json!({ "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 1 } }, "newText": "x" });
        let ws: WorkspaceEdit = serde_json::from_value(json!({
            "documentChanges": [
                { "textDocument": { "uri": path_to_uri(&b).unwrap(), "version": null }, "edits": [edit(0)] },
                { "kind": "create", "uri": path_to_uri(&a).unwrap() },
                { "textDocument": { "uri": path_to_uri(&a).unwrap(), "version": 3 }, "edits": [edit(1), edit(2)] },
            ]
        }))
        .unwrap();
        let (files, skipped) = workspace_edit_files(ws);
        assert_eq!(skipped, 1);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, a);
        assert_eq!(files[0].1.len(), 2);
        assert_eq!(files[1].0, b);
    }

    #[test]
    fn hover_markdown_formats() {
        let h: HoverContents = serde_json::from_value(json!([
            { "language": "rust", "value": "fn f()" },
            "Some docs",
        ]))
        .unwrap();
        assert_eq!(
            hover_markdown(h),
            "```rust\nfn f()\n```\n\n---\n\nSome docs"
        );
    }
}

/// Runs the client against a real server. Needs `clangd` on the machine:
/// `cargo test -- --ignored clangd`.
#[cfg(test)]
mod clangd_tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::lsp::{catalog, detect};

    fn wait_for<T>(lsp: &mut Lsp, mut pick: impl FnMut(&mut Lsp, LspEvent) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            for e in lsp.poll() {
                if let Some(v) = pick(lsp, e) {
                    return v;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for clangd");
    }

    #[test]
    #[ignore = "needs clangd installed"]
    fn clangd_end_to_end() {
        let spec = catalog::find("clangd").unwrap();
        let exe = detect::detect_server(spec).expect("clangd is not installed");
        let dir = std::env::temp_dir().join(format!("forge-clangd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.c");
        let text = "int add(int a, int b) { return a + b; }\nint main(void) {\n  int total = add(1, 2);\n  return totl;\n}\n";
        std::fs::write(&file, text).unwrap();
        let find = |name: &str| name.ends_with(".c").then(|| (spec, exe.clone()));

        let mut lsp = Lsp::new(egui::Context::default());
        lsp.sync(&[(&file, text)], Some(&dir), find);

        // Diagnostics: the typo `totl` is an error.
        let deadline = Instant::now() + Duration::from_secs(30);
        let problems = loop {
            lsp.poll();
            let p = lsp.problems(&file, text);
            if !p.is_empty() {
                break p;
            }
            assert!(Instant::now() < deadline, "no diagnostics from clangd");
            std::thread::sleep(Duration::from_millis(20));
        };
        let typo = text.find("totl").unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.severity == Severity::Error && p.range.start == typo),
            "{problems:?}"
        );
        println!("diagnostics: {problems:?}");

        // Hover over the call to `add`.
        let call = text.find("add(1").unwrap() + 1;
        lsp.hover(&file, text, call).unwrap();
        let md = wait_for(&mut lsp, |_, e| match e {
            LspEvent::Hover { markdown, .. } => Some(markdown),
            _ => None,
        });
        println!("hover: {md:?}");
        assert!(md.unwrap().contains("add"));

        // Go to definition jumps to the function's name on line 1.
        lsp.definition(&file, text, call).unwrap();
        let loc = wait_for(&mut lsp, |_, e| match e {
            LspEvent::Locations { locations, .. } => Some(locations),
            _ => None,
        });
        assert_eq!(
            loc[0].path.canonicalize().unwrap(),
            file.canonicalize().unwrap()
        );
        assert_eq!(loc[0].chars(text), 4..7);

        // Completion after typing `to` inside main offers `total`.
        let edited = text.replace("return totl;", "return to");
        let at = edited.find("return to").unwrap() + "return to".len();
        lsp.completion(&file, &edited, at, None).unwrap();
        let items = wait_for(&mut lsp, |_, e| match e {
            LspEvent::Completion { items, .. } => Some(items),
            _ => None,
        });
        assert!(
            items.iter().any(|i| i.label.trim() == "total"),
            "{:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );

        // Rename `add` everywhere.
        lsp.rename(&file, &edited, call, "sum").unwrap();
        let edit = wait_for(&mut lsp, |_, e| match e {
            LspEvent::WorkspaceEdit { edit, encoding, .. } => Some((edit, encoding)),
            _ => None,
        });
        let (files, _) = workspace_edit_files(edit.0);
        let renamed = crate::lsp::text::apply_edits(
            &edited,
            &crate::lsp::text::char_edits(&edited, &files[0].1, edit.1),
        );
        assert!(
            renamed.contains("int sum(int a") && renamed.contains("= sum(1, 2)"),
            "{renamed}"
        );

        // Formatting.
        let messy = "int   main(void){return 0;}\n";
        lsp.sync(&[(&file, messy)], Some(&dir), find);
        lsp.format(&file, messy, &FormatOptions::from_indent("    ", 4), false)
            .unwrap();
        let (edits, enc) = wait_for(&mut lsp, |_, e| match e {
            LspEvent::Format {
                edits, encoding, ..
            } => Some((edits, encoding)),
            _ => None,
        });
        let formatted =
            crate::lsp::text::apply_edits(messy, &crate::lsp::text::char_edits(messy, &edits, enc));
        println!("formatted: {formatted:?}");
        assert_ne!(formatted, messy);
        assert!(formatted.starts_with("int main(void)"));

        drop(lsp);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

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
use std::sync::Arc;
use std::time::{Duration, Instant};

use lsp_types::{
    CodeAction, CodeActionOrCommand, CodeActionProviderCapability, CompletionItem,
    CompletionResponse, Diagnostic, DiagnosticSeverity, DocumentChangeOperation, DocumentChanges,
    DocumentSymbolResponse, GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability,
    InlayHint, MarkedString, MarkupKind, NumberOrString, OneOf, ProgressParams,
    ProgressParamsValue, PublishDiagnosticsParams, SemanticTokensFullOptions,
    SemanticTokensOptions, SemanticTokensResult, SemanticTokensServerCapabilities,
    ServerCapabilities, ShowMessageParams, SignatureHelp, TextDocumentSyncCapability,
    TextDocumentSyncSaveOptions, TextEdit, Uri, WorkDoneProgress, WorkspaceEdit,
    WorkspaceSymbolResponse,
};
use serde_json::{Value, json};

use super::catalog::ServerSpec;
use super::client::{Client, ClientState};
use super::features::{
    DocSymbol, Hint, SemanticSpan, SymbolLocation, decode_document_symbols, decode_inlay_hints,
    decode_semantic_tokens, decode_workspace_symbols,
};
use super::rpc::{self, Incoming, ResponseError};
use super::text::{Encoding, LineIndex, language_id, path_to_uri, uri_to_path};

/// How long a file must stay unchanged before its views are asked for again.
const VIEW_DELAY: Duration = Duration::from_millis(250);

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
    Folding,
    CodeActions,
    SignatureHelp,
    DocumentSymbols,
    WorkspaceSymbols,
    InlayHints,
    SemanticTokens,
}

/// How long typing has to pause before fold ranges are asked for again.
const FOLDING_DEBOUNCE: Duration = Duration::from_millis(400);

impl Feature {
    fn describe(self) -> &'static str {
        match self {
            Feature::Hover => "hover",
            Feature::Definition => "go to definition",
            Feature::References => "find references",
            Feature::Completion => "completion",
            Feature::Rename => "rename",
            Feature::Formatting => "formatting",
            Feature::Folding => "folding",
            Feature::CodeActions => "code actions",
            Feature::SignatureHelp => "signature help",
            Feature::DocumentSymbols => "document symbols",
            Feature::WorkspaceSymbols => "workspace symbols",
            Feature::InlayHints => "inlay hints",
            Feature::SemanticTokens => "semantic highlighting",
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
            Feature::Folding => !matches!(
                caps.folding_range_provider,
                None | Some(lsp_types::FoldingRangeProviderCapability::Simple(false))
            ),
            Feature::CodeActions => matches!(
                caps.code_action_provider,
                Some(
                    CodeActionProviderCapability::Simple(true)
                        | CodeActionProviderCapability::Options(_)
                )
            ),
            Feature::SignatureHelp => caps.signature_help_provider.is_some(),
            Feature::DocumentSymbols => enabled(&caps.document_symbol_provider),
            Feature::WorkspaceSymbols => enabled(&caps.workspace_symbol_provider),
            Feature::InlayHints => enabled(&caps.inlay_hint_provider),
            Feature::SemanticTokens => semantic_options(caps).is_some(),
        }
    }
}

/// Semantic token options, if the server can send tokens for a whole file.
fn semantic_options(caps: &ServerCapabilities) -> Option<&SemanticTokensOptions> {
    let options = match caps.semantic_tokens_provider.as_ref()? {
        SemanticTokensServerCapabilities::SemanticTokensOptions(o) => o,
        SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(r) => {
            &r.semantic_tokens_options
        }
    };
    matches!(
        options.full,
        Some(SemanticTokensFullOptions::Bool(true) | SemanticTokensFullOptions::Delta { .. })
    )
    .then_some(options)
}

/// Information the editor keeps showing for a file and refreshes as it
/// changes, rather than asking for on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    SemanticTokens,
    InlayHints,
    Symbols,
}

impl View {
    fn index(self) -> usize {
        self as usize
    }

    fn feature(self) -> Feature {
        match self {
            View::SemanticTokens => Feature::SemanticTokens,
            View::InlayHints => Feature::InlayHints,
            View::Symbols => Feature::DocumentSymbols,
        }
    }

    fn method(self) -> &'static str {
        match self {
            View::SemanticTokens => "textDocument/semanticTokens/full",
            View::InlayHints => "textDocument/inlayHint",
            View::Symbols => "textDocument/documentSymbol",
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
    /// Code actions available at `at` in `path`.
    CodeActions {
        path: PathBuf,
        at: usize,
        actions: Vec<CodeActionOrCommand>,
        encoding: Encoding,
    },
    /// A code action with its edit filled in.
    CodeActionResolved {
        path: PathBuf,
        action: CodeAction,
        encoding: Encoding,
    },
    /// Signatures of the call around `at`, or `None` outside of one.
    SignatureHelp {
        path: PathBuf,
        at: usize,
        help: Option<SignatureHelp>,
        encoding: Encoding,
    },
    /// Ranges in these events are chars in `text`, the file as it was when
    /// asked; it may have changed since.
    SemanticTokens {
        path: PathBuf,
        text: Arc<str>,
        tokens: Vec<SemanticSpan>,
    },
    InlayHints {
        path: PathBuf,
        text: Arc<str>,
        hints: Vec<Hint>,
    },
    DocumentSymbols {
        path: PathBuf,
        text: Arc<str>,
        symbols: Vec<DocSymbol>,
        /// Asked for by Go to Symbol, which is waiting for them.
        open_picker: bool,
    },
    /// Matches for `query` from one server.
    WorkspaceSymbols {
        query: String,
        symbols: Vec<SymbolLocation>,
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
    /// Fold ranges `(start, end)` for the text at `folds_for` (a version).
    folds: Option<Vec<(usize, usize)>>,
    folds_for: i32,
    /// Version a fold request is outstanding for.
    folds_requested: Option<i32>,
    /// When the text last changed, to wait for typing to pause.
    changed: Option<Instant>,
    /// The version each [`View`] was last asked for at.
    views: [Option<i32>; 3],
}

enum Pending {
    Hover {
        path: PathBuf,
        at: usize,
    },
    Locations(LocationKind),
    Completion {
        path: PathBuf,
        at: usize,
    },
    Rename,
    Folding {
        path: PathBuf,
        version: i32,
    },
    Format {
        path: PathBuf,
        then_save: bool,
    },
    CodeActions {
        path: PathBuf,
        at: usize,
    },
    ResolveCodeAction {
        path: PathBuf,
    },
    ExecuteCommand,
    SignatureHelp {
        path: PathBuf,
        at: usize,
    },
    View {
        path: PathBuf,
        view: View,
        text: Arc<str>,
        open_picker: bool,
    },
    WorkspaceSymbols {
        query: String,
    },
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
                self.request_folds(path);
            } else {
                self.open(path, text, root, &find_server);
            }
        }
    }

    /// Asks for fold ranges when the text changed and typing has paused.
    fn request_folds(&mut self, path: &Path) {
        if !self.supports(path, Feature::Folding) {
            return;
        }
        let Some(doc) = self.docs.get(path) else {
            return;
        };
        if doc.folds_requested.is_some() || (doc.folds.is_some() && doc.folds_for == doc.version) {
            return;
        }
        let waited = doc.changed.map_or(FOLDING_DEBOUNCE, |t| t.elapsed());
        if doc.folds.is_some() && waited < FOLDING_DEBOUNCE {
            self.ctx.request_repaint_after(FOLDING_DEBOUNCE - waited);
            return;
        }
        let (key, version) = (doc.key.clone(), doc.version);
        let params = json!({ "textDocument": { "uri": doc.uri } });
        let pending = Pending::Folding {
            path: path.to_path_buf(),
            version,
        };
        if let Some(doc) = self.docs.get_mut(path) {
            doc.folds_requested = Some(version);
        }
        self.send_request(key, "textDocument/foldingRange", params, pending, false);
    }

    /// The fold ranges the server reported for `path` (`(start, end)` lines,
    /// sorted), or `None` if its server doesn't do folding.
    pub fn folding_ranges(&self, path: &Path) -> Option<Vec<(usize, usize)>> {
        if !self.supports(path, Feature::Folding) {
            return None;
        }
        self.docs.get(path)?.folds.clone()
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
                changed: None,
                views: [None; 3],
                folds: None,
                folds_for: -1,
                folds_requested: None,
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
        doc.changed = Some(Instant::now());
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
            && let Some(old) = self.pending.remove(&(old_key.clone(), old_id))
        {
            self.not_sent(&old);
            if let Some(client) = self.clients.get_mut(&old_key) {
                client.cancel(old_id);
            }
        }
        self.pending.insert((key, id), pending);
    }

    /// A view request was dropped before it was answered: ask again later.
    fn not_sent(&mut self, pending: &Pending) {
        if let Pending::View { path, view, .. } = pending
            && let Some(doc) = self.docs.get_mut(path)
        {
            doc.views[view.index()] = None;
        }
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

    /// Asks for code actions (quick fixes, refactors) for `range`, telling the
    /// server which diagnostics are there.
    pub fn code_actions(
        &mut self,
        path: &Path,
        text: &str,
        range: Range<usize>,
    ) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::CodeActions)?;
        let at = range.start;
        let index = LineIndex::new(text);
        let range = lsp_types::Range::new(
            index.position_of(range.start, enc),
            index.position_of(range.end, enc),
        );
        let diagnostics: Vec<&Diagnostic> = self
            .diagnostics
            .get(path)
            .filter(|(k, _)| *k == key)
            .map(|(_, all)| {
                all.iter()
                    .filter(|d| d.range.start <= range.end && range.start <= d.range.end)
                    .collect()
            })
            .unwrap_or_default();
        let params = json!({
            "textDocument": { "uri": uri },
            "range": range,
            "context": { "diagnostics": diagnostics, "triggerKind": 1 },
        });
        let pending = Pending::CodeActions {
            path: path.to_path_buf(),
            at,
        };
        self.send_request(key, "textDocument/codeAction", params, pending, true);
        Ok(())
    }

    /// Asks the server to fill in a code action's edit. Returns false if the
    /// server can't, in which case the action has nothing to apply.
    pub fn resolve_code_action(
        &mut self,
        path: &Path,
        text: &str,
        action: &CodeAction,
    ) -> Result<bool, String> {
        let (key, _, _) = self.prepare(path, text, Feature::CodeActions)?;
        let resolvable = matches!(
            &self.clients[&key].capabilities().code_action_provider,
            Some(CodeActionProviderCapability::Options(o)) if o.resolve_provider == Some(true)
        );
        if !resolvable {
            return Ok(false);
        }
        let params = serde_json::to_value(action).unwrap_or_default();
        let pending = Pending::ResolveCodeAction {
            path: path.to_path_buf(),
        };
        self.send_request(key, "codeAction/resolve", params, pending, false);
        Ok(true)
    }

    /// Runs a server command, e.g. from a code action. Any edits it makes
    /// arrive as `workspace/applyEdit` requests.
    pub fn execute_command(
        &mut self,
        path: &Path,
        text: &str,
        command: &lsp_types::Command,
    ) -> Result<(), String> {
        let (key, _, _) = self.prepare(path, text, Feature::CodeActions)?;
        let params = json!({
            "command": command.command,
            "arguments": command.arguments.clone().unwrap_or_default(),
        });
        self.send_request(
            key,
            "workspace/executeCommand",
            params,
            Pending::ExecuteCommand,
            false,
        );
        Ok(())
    }

    /// Asks for the signature of the call around `at`. `trigger` is the
    /// character just typed, if it is a trigger; `retrigger` means a popup is
    /// already showing and is being kept up to date.
    pub fn signature_help(
        &mut self,
        path: &Path,
        text: &str,
        at: usize,
        trigger: Option<char>,
        retrigger: bool,
    ) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::SignatureHelp)?;
        let mut params = Self::position_params(&uri, text, at, enc);
        // Invoked = 1, typed a trigger character = 2, content changed = 3.
        let kind = match (trigger, retrigger) {
            (Some(_), _) => 2,
            (None, true) => 3,
            (None, false) => 1,
        };
        params["context"] = json!({ "triggerKind": kind, "isRetrigger": retrigger });
        if let Some(c) = trigger {
            params["context"]["triggerCharacter"] = json!(c.to_string());
        }
        let pending = Pending::SignatureHelp {
            path: path.to_path_buf(),
            at,
        };
        self.send_request(key, "textDocument/signatureHelp", params, pending, true);
        Ok(())
    }

    /// Characters that open signature help, and ones that update it while
    /// it is open (e.g. `(` and `,`).
    pub fn signature_triggers(&self, path: &Path) -> (Vec<char>, Vec<char>) {
        let Some(options) = self
            .docs
            .get(path)
            .and_then(|d| self.clients.get(&d.key))
            .and_then(|c| c.capabilities().signature_help_provider.as_ref())
        else {
            return Default::default();
        };
        let chars = |list: &Option<Vec<String>>| -> Vec<char> {
            list.iter()
                .flatten()
                .filter_map(|s| s.chars().last())
                .collect()
        };
        (
            chars(&options.trigger_characters),
            chars(&options.retrigger_characters),
        )
    }

    /// Asks for the file's symbols now (Go to Symbol), rather than waiting
    /// for [`Lsp::refresh_views`].
    pub fn document_symbols(&mut self, path: &Path, text: &str) -> Result<(), String> {
        let (key, uri, enc) = self.prepare(path, text, Feature::DocumentSymbols)?;
        let version = self.docs[path].version;
        self.request_view(key, path, uri, enc, Arc::from(text), View::Symbols, true);
        self.docs.get_mut(path).expect("prepared above").views[View::Symbols.index()] =
            Some(version);
        Ok(())
    }

    /// Searches every running server's workspace for symbols matching
    /// `query`. Returns how many servers were asked.
    pub fn workspace_symbols(&mut self, query: &str) -> usize {
        // Answers to an older query are no longer wanted.
        let stale: Vec<(ServerKey, i64)> = self
            .pending
            .iter()
            .filter(|(_, p)| matches!(p, Pending::WorkspaceSymbols { .. }))
            .map(|(k, _)| k.clone())
            .collect();
        for (key, id) in stale {
            self.pending.remove(&(key.clone(), id));
            if let Some(client) = self.clients.get_mut(&key) {
                client.cancel(id);
            }
        }
        let keys: Vec<ServerKey> = self
            .clients
            .iter()
            .filter(|(_, c)| {
                c.is_ready() && Feature::WorkspaceSymbols.supported_by(c.capabilities())
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in &keys {
            let pending = Pending::WorkspaceSymbols {
                query: query.to_owned(),
            };
            let params = json!({ "query": query });
            self.send_request(key.clone(), "workspace/symbol", params, pending, false);
        }
        keys.len()
    }

    /// Keeps `path`'s views (semantic tokens, inlay hints, symbols) up to
    /// date: asks for the ones in `views` once the text has stopped changing
    /// for a moment. Call every frame for the file being shown.
    pub fn refresh_views(&mut self, path: &Path, views: &[View]) {
        let Some(doc) = self.docs.get(path) else {
            return;
        };
        let Some(client) = self.clients.get(&doc.key) else {
            return;
        };
        if !client.is_ready() {
            return;
        }
        let due: Vec<View> = views
            .iter()
            .copied()
            .filter(|v| {
                doc.views[v.index()] != Some(doc.version)
                    && v.feature().supported_by(client.capabilities())
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let wait = doc
            .changed
            .map_or(Duration::ZERO, |t| VIEW_DELAY.saturating_sub(t.elapsed()));
        if !wait.is_zero() {
            self.ctx.request_repaint_after(wait);
            return;
        }
        let (key, uri, version, enc) = (
            doc.key.clone(),
            doc.uri.clone(),
            doc.version,
            client.encoding,
        );
        let text: Arc<str> = Arc::from(doc.text.as_str());
        for view in due {
            self.request_view(
                key.clone(),
                path,
                uri.clone(),
                enc,
                Arc::clone(&text),
                view,
                false,
            );
            if let Some(doc) = self.docs.get_mut(path) {
                doc.views[view.index()] = Some(version);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn request_view(
        &mut self,
        key: ServerKey,
        path: &Path,
        uri: Uri,
        enc: Encoding,
        text: Arc<str>,
        view: View,
        open_picker: bool,
    ) {
        let mut params = json!({ "textDocument": { "uri": uri } });
        if view == View::InlayHints {
            let index = LineIndex::new(&text);
            let end = index.position_of(usize::MAX, enc);
            params["range"] = json!({ "start": { "line": 0, "character": 0 }, "end": end });
        }
        let pending = Pending::View {
            path: path.to_path_buf(),
            view,
            text,
            open_picker,
        };
        self.send_request(key, view.method(), params, pending, true);
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
            Pending::Hover { .. }
            | Pending::Completion { .. }
            | Pending::SignatureHelp { .. }
            | Pending::WorkspaceSymbols { .. }
            | Pending::View {
                open_picker: false, ..
            } => return,
            Pending::View { .. } => "Go to symbol",
            Pending::CodeActions { .. } => "Code actions",
            Pending::ResolveCodeAction { .. } | Pending::ExecuteCommand => "Code action",
            Pending::Folding { path, .. } => {
                // Don't ask again for this text; folding falls back to indentation.
                if let Some(doc) = self.docs.get_mut(&path) {
                    doc.folds_requested = None;
                    doc.folds_for = doc.version;
                }
                return;
            }
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
            Pending::Folding { path, version } => {
                let Some(doc) = self.docs.get_mut(&path) else {
                    return;
                };
                doc.folds_requested = None;
                doc.folds_for = version;
                let ranges: Vec<lsp_types::FoldingRange> = serde_json::from_value(value)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                doc.folds = Some(fold_lines(&ranges));
                self.ctx.request_repaint();
            }
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
            Pending::CodeActions { path, at } => {
                let actions = serde_json::from_value::<Option<Vec<CodeActionOrCommand>>>(value)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                self.events.push(LspEvent::CodeActions {
                    path,
                    at,
                    actions,
                    encoding,
                });
            }
            Pending::ResolveCodeAction { path } => {
                match serde_json::from_value::<CodeAction>(value) {
                    Ok(action) => self.events.push(LspEvent::CodeActionResolved {
                        path,
                        action,
                        encoding,
                    }),
                    Err(e) => self.events.push(LspEvent::Message(format!(
                        "Code action failed: unexpected reply ({e})"
                    ))),
                }
            }
            Pending::ExecuteCommand => {}
            Pending::SignatureHelp { path, at } => {
                let help = serde_json::from_value::<Option<SignatureHelp>>(value)
                    .ok()
                    .flatten()
                    .filter(|h| !h.signatures.is_empty());
                self.events.push(LspEvent::SignatureHelp {
                    path,
                    at,
                    help,
                    encoding,
                });
            }
            Pending::View {
                path,
                view,
                text,
                open_picker,
            } => self.on_view(key, path, view, text, open_picker, value),
            Pending::WorkspaceSymbols { query } => {
                let symbols = serde_json::from_value::<Option<WorkspaceSymbolResponse>>(value)
                    .ok()
                    .flatten()
                    .map(|r| decode_workspace_symbols(r, encoding))
                    .unwrap_or_default();
                self.events
                    .push(LspEvent::WorkspaceSymbols { query, symbols });
            }
        }
    }

    fn on_view(
        &mut self,
        key: &ServerKey,
        path: PathBuf,
        view: View,
        text: Arc<str>,
        open_picker: bool,
        value: Value,
    ) {
        let encoding = self.encoding(key);
        let event = match view {
            View::SemanticTokens => {
                let Some(legend) = self
                    .clients
                    .get(key)
                    .and_then(|c| semantic_options(c.capabilities()))
                    .map(|o| o.legend.clone())
                else {
                    return;
                };
                let Ok(Some(SemanticTokensResult::Tokens(tokens))) =
                    serde_json::from_value::<Option<SemanticTokensResult>>(value)
                else {
                    return;
                };
                let tokens = decode_semantic_tokens(&text, &tokens.data, &legend, encoding);
                LspEvent::SemanticTokens { path, text, tokens }
            }
            View::InlayHints => {
                let hints = serde_json::from_value::<Option<Vec<InlayHint>>>(value)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let hints = decode_inlay_hints(&text, hints, encoding);
                LspEvent::InlayHints { path, text, hints }
            }
            View::Symbols => {
                let symbols = serde_json::from_value::<Option<DocumentSymbolResponse>>(value)
                    .ok()
                    .flatten()
                    .map(|r| decode_document_symbols(&text, r, encoding))
                    .unwrap_or_default();
                LspEvent::DocumentSymbols {
                    path,
                    text,
                    symbols,
                    open_picker,
                }
            }
        };
        self.events.push(event);
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
            // Something changed project-wide (e.g. indexing finished): ask
            // again for what is on screen.
            "workspace/semanticTokens/refresh" | "workspace/inlayHint/refresh" => {
                let view = if method.starts_with("workspace/semantic") {
                    View::SemanticTokens
                } else {
                    View::InlayHints
                };
                for doc in self.docs.values_mut().filter(|d| d.key == *key) {
                    doc.views[view.index()] = None;
                }
                self.ctx.request_repaint();
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

/// Fold ranges as `(start, end)` lines, sorted by start: folding hides
/// lines `start + 1..=end`. With `lineFoldingOnly`, servers end a block's
/// range on the line before its closing bracket, so that stays visible.
fn fold_lines(ranges: &[lsp_types::FoldingRange]) -> Vec<(usize, usize)> {
    let mut lines: Vec<(usize, usize)> = ranges
        .iter()
        .filter(|r| r.end_line > r.start_line)
        .map(|r| (r.start_line as usize, r.end_line as usize))
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
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

    /// A fake server for the newer features: views, code actions, signature
    /// help and workspace symbols. After the first semantic tokens request it
    /// asks the client to refresh them.
    fn views_lsp(log: Arc<Mutex<Vec<Value>>>) -> Lsp {
        let spawner: Spawner = Box::new(move |spec, _program, root, ctx| {
            let log = Arc::clone(&log);
            let main_uri = path_to_uri(&root.join("main.rs")).unwrap();
            let uri_of_main = main_uri.as_str().to_owned();
            let mut refreshed = false;
            let (r, w) = fake_server(move |msg| {
                log.lock().unwrap().push(msg.clone());
                let id = msg["id"].clone();
                let uri = msg["params"]["textDocument"]["uri"].clone();
                let range = |l: u32, a: u32, b: u32| json!({ "start": { "line": l, "character": a }, "end": { "line": l, "character": b } });
                match msg["method"].as_str() {
                    Some("initialize") => init_reply(
                        msg,
                        json!({
                            "semanticTokensProvider": {
                                "legend": { "tokenTypes": ["function"], "tokenModifiers": [] },
                                "full": true,
                            },
                            "inlayHintProvider": true,
                            "documentSymbolProvider": true,
                            "workspaceSymbolProvider": true,
                            "codeActionProvider": { "resolveProvider": true },
                            "signatureHelpProvider": { "triggerCharacters": ["("], "retriggerCharacters": [","] },
                        }),
                    ),
                    Some("textDocument/didOpen") => vec![rpc::notification(
                        "textDocument/publishDiagnostics",
                        json!({ "uri": uri, "diagnostics": [
                            { "range": range(0, 3, 6), "severity": 1, "message": "bad" },
                            { "range": range(1, 0, 1), "severity": 2, "message": "elsewhere" },
                        ] }),
                    )],
                    Some("textDocument/semanticTokens/full") => {
                        let mut out =
                            vec![rpc::response(id, Ok(json!({ "data": [0, 3, 3, 0, 0] })))];
                        if !refreshed {
                            refreshed = true;
                            out.push(rpc::request(
                                900,
                                "workspace/semanticTokens/refresh",
                                Value::Null,
                            ));
                        }
                        out
                    }
                    Some("textDocument/inlayHint") => vec![rpc::response(
                        id,
                        Ok(
                            json!([{ "position": { "line": 0, "character": 6 }, "label": ": i32", "kind": 1 }]),
                        ),
                    )],
                    Some("textDocument/documentSymbol") => vec![rpc::response(
                        id,
                        Ok(json!([{ "name": "add", "kind": 12, "range": range(0, 0, 9),
                            "selectionRange": range(0, 3, 6) }])),
                    )],
                    Some("textDocument/codeAction") => vec![rpc::response(
                        id,
                        Ok(json!([
                            { "title": "Fix it", "kind": "quickfix", "isPreferred": true,
                              "diagnostics": msg["params"]["context"]["diagnostics"].clone() },
                            { "title": "Run", "command": "x.run" },
                        ])),
                    )],
                    Some("codeAction/resolve") => {
                        let mut action = msg["params"].clone();
                        action["edit"] = json!({ "changes": { uri_of_main.clone(): [
                            { "range": range(0, 3, 6), "newText": "sum" },
                        ] } });
                        vec![rpc::response(id, Ok(action))]
                    }
                    Some("textDocument/signatureHelp") => vec![rpc::response(
                        id,
                        Ok(json!({ "signatures": [{ "label": "add(a, b)",
                            "parameters": [{ "label": [4, 5] }, { "label": [7, 8] }] }],
                            "activeParameter": 1 })),
                    )],
                    Some("workspace/symbol") => vec![rpc::response(
                        id,
                        Ok(json!([{ "name": "add", "kind": 12,
                            "location": { "uri": main_uri.clone(), "range": range(0, 3, 6) } }])),
                    )],
                    _ => Vec::new(),
                }
            });
            Ok(Client::connect(spec, root, r, w, ctx))
        });
        Lsp::with_spawner(egui::Context::default(), spawner)
    }

    #[test]
    fn keeps_views_fresh_and_answers_newer_requests() {
        let root = std::env::temp_dir().join("forge-lsp-views-test");
        let file = root.join("main.rs");
        let text = "fn add() {}\nx\n";
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut lsp = views_lsp(Arc::clone(&log));
        let all = [View::SemanticTokens, View::InlayHints, View::Symbols];
        lsp.sync(&[(&file, text)], Some(&root), find_rust);

        // All three views arrive once the server is ready, then semantic
        // tokens again after the server's refresh request.
        let (mut tokens, mut hints, mut symbols) = (0, None, None);
        poll_until(&mut lsp, |l, events| {
            l.refresh_views(&file, &all);
            for e in events {
                match e {
                    LspEvent::SemanticTokens {
                        tokens: t,
                        text: at,
                        ..
                    } => {
                        assert_eq!(&**at, text);
                        assert_eq!(t[0].range, 3..6);
                        tokens += 1;
                    }
                    LspEvent::InlayHints { hints: h, .. } => hints = Some(h[0].at),
                    LspEvent::DocumentSymbols {
                        symbols: s,
                        open_picker,
                        ..
                    } => {
                        assert!(!open_picker);
                        symbols = Some(s[0].selection.clone());
                    }
                    _ => {}
                }
            }
            tokens >= 2 && hints.is_some() && symbols.is_some()
        });
        assert_eq!(hints, Some(6));
        assert_eq!(symbols, Some(3..6));

        // Nothing changed, so nothing more is asked for.
        let count = |log: &Arc<Mutex<Vec<Value>>>, method: &str| {
            log.lock()
                .unwrap()
                .iter()
                .filter(|m| m["method"] == method)
                .count()
        };
        lsp.refresh_views(&file, &all);
        assert_eq!(count(&log, "textDocument/semanticTokens/full"), 2);
        assert_eq!(count(&log, "textDocument/inlayHint"), 1);

        // An edit is asked about only after typing pauses.
        let edited = "fn add() {} \nx\n";
        lsp.sync(&[(&file, edited)], Some(&root), find_rust);
        lsp.refresh_views(&file, &all);
        assert_eq!(count(&log, "textDocument/inlayHint"), 1, "debounced");
        std::thread::sleep(VIEW_DELAY + Duration::from_millis(20));
        lsp.refresh_views(&file, &all);
        // The fake server logs requests on its own thread.
        poll_until(&mut lsp, |_, _| count(&log, "textDocument/inlayHint") == 2);

        // Code actions carry the diagnostics under the range.
        lsp.code_actions(&file, edited, 4..4).unwrap();
        let mut actions = None;
        poll_until(&mut lsp, |_, events| {
            for e in events {
                if let LspEvent::CodeActions { actions: a, at, .. } = e {
                    assert_eq!(*at, 4);
                    actions = Some(a.len());
                }
            }
            actions.is_some()
        });
        assert_eq!(actions, Some(2));
        let request = log
            .lock()
            .unwrap()
            .iter()
            .find(|m| m["method"] == "textDocument/codeAction")
            .cloned()
            .unwrap();
        let diagnostics = request["params"]["context"]["diagnostics"]
            .as_array()
            .unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["message"], "bad");

        // An action without an edit is resolved to get one.
        let bare: CodeAction =
            serde_json::from_value(json!({ "title": "Rename to sum", "data": 7 })).unwrap();
        assert!(lsp.resolve_code_action(&file, edited, &bare).unwrap());
        let mut resolved = None;
        poll_until(&mut lsp, |_, events| {
            for e in events {
                if let LspEvent::CodeActionResolved { action, .. } = e {
                    resolved = Some(action.clone());
                }
            }
            resolved.is_some()
        });
        let resolved = resolved.unwrap();
        assert_eq!(
            resolved.data,
            Some(json!(7)),
            "the server gets its data back"
        );
        let (files, _) = workspace_edit_files(resolved.edit.unwrap());
        assert_eq!(files[0].0, file);
        assert_eq!(files[0].1[0].new_text, "sum");

        // Signature help, opened by `(`.
        assert_eq!(lsp.signature_triggers(&file), (vec!['('], vec![',']));
        lsp.signature_help(&file, edited, 7, Some('('), false)
            .unwrap();
        let mut help = None;
        poll_until(&mut lsp, |_, events| {
            for e in events {
                if let LspEvent::SignatureHelp { help: h, .. } = e {
                    help = h.clone();
                }
            }
            help.is_some()
        });
        assert_eq!(help.unwrap().active_parameter, Some(1));

        // Workspace symbols from every server that has them.
        assert_eq!(lsp.workspace_symbols("ad"), 1);
        let mut found = None;
        poll_until(&mut lsp, |_, events| {
            for e in events {
                if let LspEvent::WorkspaceSymbols { query, symbols } = e {
                    assert_eq!(query, "ad");
                    found = symbols
                        .first()
                        .map(|s| (s.name.clone(), s.location.path.clone()));
                }
            }
            found.is_some()
        });
        assert_eq!(found, Some(("add".to_owned(), file.clone())));
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

    #[test]
    #[ignore = "needs clangd installed"]
    fn clangd_code_actions_symbols_hints_and_tokens() {
        let spec = catalog::find("clangd").unwrap();
        let exe = detect::detect_server(spec).expect("clangd is not installed");
        let dir = std::env::temp_dir().join(format!("forge-clangd-views-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.c");
        let text = "int add(int a, int b) { return a + b; }\nint main(void) {\n  int total = add(1, 2);\n  return totl;\n}\n";
        std::fs::write(&file, text).unwrap();
        let find = |name: &str| name.ends_with(".c").then(|| (spec, exe.clone()));

        let mut lsp = Lsp::new(egui::Context::default());
        lsp.sync(&[(&file, text)], Some(&dir), find);
        let deadline = Instant::now() + Duration::from_secs(30);
        while lsp.problems(&file, text).is_empty() {
            lsp.poll();
            assert!(Instant::now() < deadline, "no diagnostics from clangd");
            std::thread::sleep(Duration::from_millis(20));
        }

        // The quick fix for the typo replaces `totl` with `total`.
        let typo = text.find("totl").unwrap();
        lsp.code_actions(&file, text, typo..typo).unwrap();
        let (actions, enc) = wait_for(&mut lsp, |_, e| match e {
            LspEvent::CodeActions {
                actions, encoding, ..
            } => Some((actions, encoding)),
            _ => None,
        });
        println!("code actions: {actions:?}");
        let fix = actions
            .into_iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(a) if a.edit.is_some() => Some(a),
                _ => None,
            })
            .expect("a quick fix with an edit");
        let (files, _) = workspace_edit_files(fix.edit.unwrap());
        let fixed = crate::lsp::text::apply_edits(
            text,
            &crate::lsp::text::char_edits(text, &files[0].1, enc),
        );
        assert!(fixed.contains("return total;"), "{fixed}");

        // Signature help inside the call, on the second argument.
        let arg = text.find("2);").unwrap();
        lsp.signature_help(&file, text, arg, None, false).unwrap();
        let help = wait_for(&mut lsp, |_, e| match e {
            LspEvent::SignatureHelp { help, encoding, .. } => Some(help.map(|h| (h, encoding))),
            _ => None,
        });
        let (help, enc) = help.expect("signature help in a call");
        let sig = &help.signatures[0];
        let param = sig.active_parameter.or(help.active_parameter).unwrap();
        let label = &sig.parameters.as_ref().unwrap()[param as usize].label;
        let range = crate::lsp::features::parameter_range(&sig.label, label, enc).unwrap();
        assert_eq!(&sig.label[range], "int b");

        // Views: symbols, inlay hints (parameter names), semantic tokens.
        let all = [View::SemanticTokens, View::InlayHints, View::Symbols];
        let (mut symbols, mut hints, mut tokens) = (None, None, None);
        let deadline = Instant::now() + Duration::from_secs(30);
        while symbols.is_none() || hints.is_none() || tokens.is_none() {
            assert!(Instant::now() < deadline, "views never arrived");
            lsp.refresh_views(&file, &all);
            for e in lsp.poll() {
                match e {
                    LspEvent::DocumentSymbols { symbols: s, .. } => symbols = Some(s),
                    LspEvent::InlayHints { hints: h, .. } => hints = Some(h),
                    LspEvent::SemanticTokens { tokens: t, .. } => tokens = Some(t),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let names: Vec<_> = symbols.unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["add", "main"]);
        let hints = hints.unwrap();
        println!("hints: {hints:?}");
        let first_arg = text.find("1, 2").unwrap();
        assert!(
            hints
                .iter()
                .any(|h| h.at == first_arg && h.label.trim().trim_end_matches(':') == "a"),
            "{hints:?}"
        );
        let tokens = tokens.unwrap();
        let call = text.find("add(1").unwrap();
        let at_call = tokens
            .iter()
            .find(|t| t.range.start == call)
            .expect("token for the call");
        assert_eq!(&*at_call.kind, "function");

        // Workspace symbols find `main` in the open file.
        assert_eq!(lsp.workspace_symbols("main"), 1);
        let found = wait_for(&mut lsp, |_, e| match e {
            LspEvent::WorkspaceSymbols { symbols, .. } => Some(symbols),
            _ => None,
        });
        assert!(found.iter().any(|s| s.name == "main"), "{found:?}");

        drop(lsp);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

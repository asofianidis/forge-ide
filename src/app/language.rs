//! Language server features in the editor: diagnostics (squiggles, status bar
//! counts, problems list), hover, go to definition, find references,
//! completion, rename and formatting.
//!
//! The protocol side lives in `crate::lsp`; this module turns its events into
//! edits, popups and cursor moves.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::text_edit::TextEditOutput;
use egui::{Align2, Color32, Key, Modifiers, Pos2, Rect, Shape, Stroke, TextStyle, pos2, vec2};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, InsertTextFormat,
};

use super::ForgeApp;
use super::editing::editor_id;
use crate::editor::ops::{self, Selection};
use crate::fuzzy;
use crate::lsp::text::{self, CharEdit, Encoding, LineIndex};
use crate::lsp::{
    Feature, Location, LocationKind, LspEvent, Problem, ServerActivity, Severity,
    workspace_edit_files,
};
use crate::palette::PickerItem;
use crate::theme::UiColors;
use crate::ui::markdown;

/// How long the pointer rests on a symbol before its hover docs are fetched.
const HOVER_DELAY: f32 = 0.35;
/// How long Save waits for format-on-save before saving unformatted.
const FORMAT_TIMEOUT: Duration = Duration::from_millis(2500);
/// Completion rows visible without scrolling.
const COMPLETION_ROWS: usize = 10;

#[derive(Default)]
pub(super) struct LanguageUi {
    hover: Option<HoverPopup>,
    completion: Option<CompletionPopup>,
    rename: Option<RenameDialog>,
    formatting: Option<FormatRequest>,
    /// Rows of the definitions / references / problems picker.
    pub(super) locations: Vec<LocationItem>,
    pub(super) locations_hint: &'static str,
}

struct HoverPopup {
    doc: u64,
    path: Option<PathBuf>,
    /// The char the pointer rested on.
    at: usize,
    /// Chars the popup is about; it stays open while the pointer is on them.
    range: Range<usize>,
    /// Screen rect of the hovered char, to place the popup next to.
    anchor: Rect,
    problems: Vec<Problem>,
    markdown: Option<String>,
    loading: bool,
    /// Where the popup was drawn last frame.
    rect: Option<Rect>,
}

struct CompletionPopup {
    doc: u64,
    /// Where the request was made.
    at: usize,
    /// Start of the word being completed; typing filters from here.
    start: usize,
    items: Vec<CompletionItem>,
    encoding: Encoding,
    incomplete: bool,
    /// What `shown` was filtered for.
    query: String,
    /// Indices into `items`, best match first.
    shown: Vec<usize>,
    selected: usize,
    /// Scroll the selection into view on the next draw.
    scroll: bool,
}

struct RenameDialog {
    path: PathBuf,
    at: usize,
    input: String,
    select_all: bool,
}

struct FormatRequest {
    path: PathBuf,
    /// The text that was sent; edits only apply to exactly this text.
    text: String,
    then_save: bool,
    started: Instant,
}

pub(super) struct LocationItem {
    pub label: String,
    pub detail: String,
    pub location: Location,
}

/// What the editor widget saw this frame, for the language features to act on.
#[derive(Default)]
pub(super) struct EditorFrame {
    /// The char under the pointer and its screen rect.
    pub pointer: Option<(usize, Rect)>,
    /// The caret's char index and screen rect.
    pub caret: Option<(usize, Rect)>,
    /// The last character typed into the buffer this frame.
    pub typed: Option<char>,
    /// Cmd/Ctrl+click on a symbol.
    pub definition_at: Option<usize>,
    pub problems: Vec<Problem>,
}

impl ForgeApp {
    // ---- per frame -------------------------------------------------------

    /// Starts servers for newly opened files and sends edits. Call once per frame.
    pub(super) fn sync_language_servers(&mut self) {
        let docs: Vec<(&Path, &str)> = self
            .documents
            .iter()
            .filter_map(|d| Some((d.path.as_deref()?, d.text())))
            .collect();
        let root = self.workspace.as_ref().map(|w| w.root.path.as_path());
        let servers = &self.servers;
        self.lsp
            .sync(&docs, root, |name| servers.installed_server_for(name));
    }

    pub(super) fn handle_lsp_events(&mut self, ctx: &egui::Context) {
        for event in self.lsp.poll() {
            match event {
                LspEvent::Hover {
                    path,
                    at,
                    markdown,
                    range,
                } => self.on_hover(&path, at, markdown, range),
                LspEvent::Locations { kind, locations } => self.on_locations(ctx, kind, locations),
                LspEvent::Completion {
                    path,
                    at,
                    items,
                    encoding,
                    incomplete,
                } => self.on_completion(ctx, path, at, items, encoding, incomplete),
                LspEvent::WorkspaceEdit {
                    label,
                    edit,
                    encoding,
                    reply,
                } => {
                    let result = self.apply_workspace_edit(ctx, label, edit, encoding);
                    let applied = result.is_ok();
                    self.status = result.unwrap_or_else(|e| format!("Error: {e}"));
                    if let Some((key, id)) = reply {
                        self.lsp.reply_apply_edit(&key, id, applied);
                    }
                }
                LspEvent::Format {
                    path,
                    edits,
                    encoding,
                    then_save,
                } => self.on_format(ctx, &path, &edits, encoding, then_save),
                LspEvent::Message(msg) => self.status = msg,
            }
        }
        // A server that never answers must not block saving.
        if let Some(req) = &self.language.formatting
            && req.then_save
            && req.started.elapsed() > FORMAT_TIMEOUT
        {
            let path = req.path.clone();
            self.language.formatting = None;
            if let Some(idx) = self.doc_index(&path) {
                self.save(idx);
                self.status = "Formatting timed out; saved without formatting".to_owned();
            }
        }
    }

    pub(super) fn restart_language_servers(&mut self) {
        self.lsp.restart();
        self.language = LanguageUi::default();
        self.status = "Restarting language servers…".to_owned();
    }

    fn doc_index(&self, path: &Path) -> Option<usize> {
        self.documents
            .iter()
            .position(|d| d.path.as_deref() == Some(path))
    }

    /// The active document's path, text and caret, if it is a file.
    fn active_file(&self, ctx: &egui::Context) -> Option<(PathBuf, usize)> {
        let doc = self.documents.get(self.active)?;
        let path = doc.path.clone()?;
        Some((path, self.selection(ctx).end))
    }

    /// Applies `edits` to a document, keeping its caret and selection on the
    /// same text.
    fn edit_document(&mut self, ctx: &egui::Context, idx: usize, edits: &[CharEdit]) {
        let Some(doc) = self.documents.get_mut(idx) else {
            return;
        };
        doc.apply_edits(edits.iter().map(|e| (e.range.clone(), e.text.as_str())));
        let id = editor_id(doc);
        if let Some(mut state) = egui::TextEdit::load_state(ctx, id)
            && let Some(range) = state.cursor.char_range()
        {
            let primary = text::map_offset(range.primary.index.into(), edits);
            let secondary = text::map_offset(range.secondary.index.into(), edits);
            state.cursor.set_char_range(Some(CCursorRange {
                primary: CCursor::new(primary),
                secondary: CCursor::new(secondary),
                h_pos: None,
            }));
            state.store(ctx, id);
            if idx == self.active {
                self.cursor = doc.line_col(primary);
            }
        }
    }

    // ---- editor hooks ----------------------------------------------------

    /// Keys for the completion list. Runs before shortcuts and the editor so
    /// that Enter, Tab and the arrows go to the list while it is open.
    pub(super) fn completion_keys(&mut self, ctx: &egui::Context) {
        if self.language.completion.is_none() {
            return;
        }
        if self.active_pane.is_some() || !self.editor_has_focus(ctx) {
            self.language.completion = None;
            return;
        }
        let (down, up, page_down, page_up, accept, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::PageDown),
                i.consume_key(Modifiers::NONE, Key::PageUp),
                i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if escape {
            self.language.completion = None;
            return;
        }
        if accept {
            self.accept_completion(ctx);
            return;
        }
        let Some(c) = &mut self.language.completion else {
            return;
        };
        let n = c.shown.len();
        if n == 0 {
            return;
        }
        let step = |by: isize, wrap: bool, sel: usize| -> usize {
            let next = sel as isize + by;
            if wrap {
                next.rem_euclid(n as isize) as usize
            } else {
                next.clamp(0, n as isize - 1) as usize
            }
        };
        let before = c.selected;
        if down {
            c.selected = step(1, true, c.selected);
        }
        if up {
            c.selected = step(-1, true, c.selected);
        }
        if page_down {
            c.selected = step(COMPLETION_ROWS as isize, false, c.selected);
        }
        if page_up {
            c.selected = step(-(COMPLETION_ROWS as isize), false, c.selected);
        }
        c.scroll |= c.selected != before;
    }

    /// Acts on what happened in the editor this frame and draws the hover and
    /// completion popups. Called at the end of `editor`.
    pub(super) fn after_editor(&mut self, ctx: &egui::Context, frame: EditorFrame) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let doc_id = doc.id;
        let path = doc.path.clone();

        if let (Some(at), Some(p)) = (frame.definition_at, &path) {
            let text = doc.text().to_owned();
            if let Err(e) = self.lsp.definition(p, &text, at) {
                self.status = e;
            }
        }
        self.update_hover(ctx, doc_id, path.as_deref(), &frame);
        if let (Some(p), Some((caret, _))) = (&path, frame.caret) {
            self.update_completion(doc_id, p, caret, frame.typed);
        } else {
            self.language.completion = None;
        }

        let colors = self.current_theme().ui.clone();
        self.show_hover(ctx, doc_id, &colors);
        if let Some((_, caret_rect)) = frame.caret {
            self.show_completion(ctx, caret_rect, &colors);
        }
    }

    // ---- hover -----------------------------------------------------------

    fn update_hover(
        &mut self,
        ctx: &egui::Context,
        doc_id: u64,
        path: Option<&Path>,
        frame: &EditorFrame,
    ) {
        let (pointer, resting) = ctx.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.time_since_last_movement() >= HOVER_DELAY && !i.pointer.any_down(),
            )
        });
        // Keep the popup while the pointer is on its symbol or inside it.
        if let Some(h) = &self.language.hover {
            let on_range = frame
                .pointer
                .as_ref()
                .is_some_and(|(at, _)| h.doc == doc_id && h.range.contains(at));
            let on_popup = h
                .rect
                .zip(pointer)
                .is_some_and(|(r, p)| r.expand(6.0).contains(p));
            if h.doc != doc_id || !(on_range || on_popup) {
                self.language.hover = None;
            }
        }
        let Some((at, anchor)) = frame.pointer else {
            return;
        };
        if self.language.hover.is_some() {
            return;
        }
        if !resting {
            // Wake up when the pointer has rested long enough.
            ctx.request_repaint_after(Duration::from_secs_f32(HOVER_DELAY + 0.02));
            return;
        }
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let problems: Vec<Problem> = frame
            .problems
            .iter()
            .filter(|p| p.range.contains(&at) || (p.range.is_empty() && p.range.start == at))
            .cloned()
            .collect();
        let word = doc
            .text()
            .chars()
            .nth(at)
            .filter(|&c| text::is_word_char(c))
            .and_then(|_| text::word_at(doc.text(), at));
        let mut range = word.clone().unwrap_or(at..at + 1);
        for p in &problems {
            range = range.start.min(p.range.start)..range.end.max(p.range.end);
        }
        let mut loading = false;
        if word.is_some()
            && let Some(p) = path
            && self.lsp.supports(p, Feature::Hover)
        {
            let text = doc.text().to_owned();
            loading = self.lsp.hover(p, &text, at).is_ok();
        }
        if !loading && problems.is_empty() {
            return;
        }
        self.language.hover = Some(HoverPopup {
            doc: doc_id,
            path: path.map(Path::to_path_buf),
            at,
            range,
            anchor,
            problems,
            markdown: None,
            loading,
            rect: None,
        });
    }

    fn on_hover(
        &mut self,
        path: &Path,
        at: usize,
        markdown: Option<String>,
        range: Option<Range<usize>>,
    ) {
        let Some(h) = &mut self.language.hover else {
            return;
        };
        if h.path.as_deref() != Some(path) || h.at != at {
            return;
        }
        h.loading = false;
        if let Some(r) = range
            && r.contains(&at)
        {
            h.range = h.range.start.min(r.start)..h.range.end.max(r.end);
        }
        h.markdown = markdown;
        if h.markdown.is_none() && h.problems.is_empty() {
            self.language.hover = None;
        }
    }

    fn show_hover(&mut self, ctx: &egui::Context, doc_id: u64, colors: &UiColors) {
        let Some(h) = &mut self.language.hover else {
            return;
        };
        if h.doc != doc_id || (h.loading && h.problems.is_empty()) {
            return;
        }
        // Above the symbol when there's room, as in other editors; else below.
        let room_above = h.anchor.top() - ctx.content_rect().top();
        let (pos, pivot) = if room_above > 160.0 {
            (h.anchor.left_top(), Align2::LEFT_BOTTOM)
        } else {
            (h.anchor.left_bottom(), Align2::LEFT_TOP)
        };
        let response = egui::Area::new(egui::Id::unique("lsp_hover"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .pivot(pivot)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(560.0);
                    for p in &h.problems {
                        problem_row(ui, p, colors);
                    }
                    if let Some(md) = &h.markdown {
                        if !h.problems.is_empty() {
                            ui.separator();
                        }
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .auto_shrink([true, true])
                            .show(ui, |ui| markdown::show(ui, md));
                    } else if h.loading {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak("Loading…");
                        });
                    }
                });
            });
        h.rect = Some(response.response.rect);
    }

    // ---- go to definition / references / problems ------------------------

    pub(super) fn go_to_definition(&mut self, ctx: &egui::Context) {
        self.position_request(ctx, |lsp, path, text, at| lsp.definition(path, text, at));
    }

    pub(super) fn find_references(&mut self, ctx: &egui::Context) {
        self.position_request(ctx, |lsp, path, text, at| lsp.references(path, text, at));
    }

    fn position_request(
        &mut self,
        ctx: &egui::Context,
        send: impl FnOnce(&mut crate::lsp::Lsp, &Path, &str, usize) -> Result<(), String>,
    ) {
        self.active_pane = None;
        let Some((path, at)) = self.active_file(ctx) else {
            self.status = "Save the file to use language features".to_owned();
            return;
        };
        let text = self.documents[self.active].text().to_owned();
        if let Err(e) = send(&mut self.lsp, &path, &text, at) {
            self.status = e;
        }
    }

    fn on_locations(&mut self, ctx: &egui::Context, kind: LocationKind, locations: Vec<Location>) {
        match (kind, locations.as_slice()) {
            (LocationKind::Definition, []) => self.status = "No definition found".to_owned(),
            (LocationKind::References, []) => self.status = "No references found".to_owned(),
            (LocationKind::Definition, [one]) => {
                let one = one.clone();
                self.go_to_location(ctx, &one);
            }
            _ => {
                let n = locations.len();
                self.language.locations = self.location_items(locations);
                self.language.locations_hint = match kind {
                    LocationKind::Definition => "Definitions",
                    LocationKind::References => "References",
                };
                self.open_locations_picker();
                if kind == LocationKind::References {
                    self.status = format!("{n} reference{}", if n == 1 { "" } else { "s" });
                }
            }
        }
    }

    /// Opens the file and puts the caret at the start of the location.
    pub(super) fn go_to_location(&mut self, ctx: &egui::Context, location: &Location) {
        self.open_file(&location.path);
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if doc.path.as_deref() != Some(location.path.as_path()) {
            return;
        }
        let at = location.chars(doc.text()).start;
        self.set_selection(ctx, Selection::caret(at), true);
    }

    /// Picker rows for locations: the line of code, and where it is.
    fn location_items(&self, locations: Vec<Location>) -> Vec<LocationItem> {
        let mut files: HashMap<PathBuf, Option<String>> = HashMap::new();
        locations
            .into_iter()
            .map(|location| {
                let text = files.entry(location.path.clone()).or_insert_with(|| {
                    match self.doc_index(&location.path) {
                        Some(i) => Some(self.documents[i].text().to_owned()),
                        None => std::fs::read_to_string(&location.path).ok(),
                    }
                });
                let line = location.range.start.line as usize;
                let label = text
                    .as_deref()
                    .and_then(|t| t.lines().nth(line))
                    .map(|l| l.trim().chars().take(160).collect())
                    .unwrap_or_default();
                LocationItem {
                    label,
                    detail: format!("{}:{}", self.display_path(&location.path), line + 1),
                    location,
                }
            })
            .collect()
    }

    fn display_path(&self, path: &Path) -> String {
        self.workspace
            .as_ref()
            .and_then(|w| path.strip_prefix(&w.root.path).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    }

    pub(super) fn show_problems(&mut self) {
        let problems = self.lsp.workspace_problems();
        if problems.is_empty() {
            self.status = "No problems".to_owned();
            return;
        }
        self.language.locations = problems
            .into_iter()
            .map(|p| LocationItem {
                label: p.message.lines().next().unwrap_or_default().to_owned(),
                detail: format!(
                    "{} {}:{}",
                    match p.severity {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                        Severity::Info => "info",
                        Severity::Hint => "hint",
                    },
                    self.display_path(&p.path),
                    p.location.range.start.line + 1
                ),
                location: p.location,
            })
            .collect();
        self.language.locations_hint = "Problems";
        self.open_locations_picker();
    }

    /// Items for the locations picker, fuzzy-filtered by `query`, with the
    /// index of each item's location.
    pub(super) fn location_picker_items(&self, query: &str) -> (Vec<PickerItem>, Vec<usize>) {
        self.language
            .locations
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let m = fuzzy::score(query, &item.label).or_else(|| {
                    fuzzy::score(query, &item.detail).map(|m| fuzzy::FuzzyMatch {
                        indices: Vec::new(),
                        ..m
                    })
                })?;
                Some((
                    PickerItem {
                        label: item.label.clone(),
                        detail: item.detail.clone(),
                        highlights: m.indices,
                    },
                    i,
                ))
            })
            .unzip()
    }

    /// Moves the caret to the next (or previous) problem in the file.
    pub(super) fn go_to_problem(&mut self, ctx: &egui::Context, forward: bool) {
        self.active_pane = None;
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let Some(path) = &doc.path else {
            return;
        };
        let mut problems = self.lsp.problems(path, doc.text());
        if problems.is_empty() {
            self.status = "No problems in this file".to_owned();
            return;
        }
        problems.sort_by_key(|p| p.range.start);
        let caret = self.selection(ctx).start;
        let target = if forward {
            problems
                .iter()
                .find(|p| p.range.start > caret)
                .or(problems.first())
        } else {
            problems
                .iter()
                .rev()
                .find(|p| p.range.start < caret)
                .or(problems.last())
        };
        let Some(p) = target.cloned() else {
            return;
        };
        self.set_selection(ctx, Selection::caret(p.range.start), true);
        let source = p.source.map(|s| format!(" [{s}]")).unwrap_or_default();
        self.status = format!(
            "{}: {}{source}",
            severity_name(p.severity),
            p.message.lines().next().unwrap_or_default()
        );
    }

    // ---- completion ------------------------------------------------------

    /// Ctrl+Space: asks for completions at the caret.
    pub(super) fn trigger_suggest(&mut self, ctx: &egui::Context) {
        let Some((path, at)) = self.active_file(ctx) else {
            return;
        };
        let text = self.documents[self.active].text().to_owned();
        if let Err(e) = self.lsp.completion(&path, &text, at, None) {
            self.status = e;
        }
    }

    /// Keeps the completion list in step with typing: filters it, closes it
    /// when the caret leaves the word, and opens it on identifier or trigger
    /// characters.
    fn update_completion(&mut self, doc_id: u64, path: &Path, caret: usize, typed: Option<char>) {
        let text = self.documents[self.active].text();
        let mut requery = None;
        if let Some(c) = &mut self.language.completion {
            let query: Option<String> = (c.doc == doc_id && caret >= c.start).then(|| {
                text.chars()
                    .skip(c.start)
                    .take(caret - c.start)
                    .collect::<String>()
            });
            match query {
                Some(q) if q.chars().all(text::is_word_char) => {
                    if q != c.query {
                        c.filter(&q);
                        if c.incomplete {
                            requery = Some(None);
                        }
                    }
                }
                _ => self.language.completion = None,
            }
        }
        if let Some(ch) = typed {
            if self.lsp.completion_triggers(path).contains(&ch) {
                requery = Some(Some(ch));
            } else if text::is_word_char(ch) && self.language.completion.is_none() {
                requery = Some(None);
            }
        }
        if let Some(trigger) = requery
            && self.lsp.supports(path, Feature::Completion)
        {
            let text = text.to_owned();
            let _ = self.lsp.completion(path, &text, caret, trigger);
        }
    }

    fn on_completion(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        at: usize,
        items: Vec<CompletionItem>,
        encoding: Encoding,
        incomplete: bool,
    ) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if doc.path.as_deref() != Some(path.as_path()) || !self.editor_has_focus(ctx) {
            return;
        }
        let caret = self.selection(ctx).end;
        let start = text::word_start(doc.text(), at);
        let query: String = doc
            .text()
            .chars()
            .skip(start)
            .take(caret.saturating_sub(start))
            .collect();
        if caret < start || !query.chars().all(text::is_word_char) {
            return;
        }
        let mut popup = CompletionPopup {
            doc: doc.id,
            at,
            start,
            items,
            encoding,
            incomplete,
            query: String::new(),
            shown: Vec::new(),
            selected: 0,
            scroll: true,
        };
        popup.filter(&query);
        self.language.completion = (!popup.shown.is_empty()).then_some(popup);
    }

    fn accept_completion(&mut self, ctx: &egui::Context) {
        let Some(c) = self.language.completion.take() else {
            return;
        };
        let Some(&item_idx) = c.shown.get(c.selected) else {
            return;
        };
        let item = &c.items[item_idx];
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if doc.id != c.doc {
            return;
        }
        let caret = self.selection(ctx).end;
        let index = LineIndex::new(doc.text());
        // Text typed since the request shifts the end of the server's range.
        let typed_since = caret.saturating_sub(c.at);
        let (range, insert) = match &item.text_edit {
            Some(CompletionTextEdit::Edit(e)) => {
                let r = index.range_of(&e.range, c.encoding);
                (
                    r.start..(r.end + typed_since).max(caret),
                    e.new_text.clone(),
                )
            }
            Some(CompletionTextEdit::InsertAndReplace(e)) => {
                let r = index.range_of(&e.insert, c.encoding);
                (
                    r.start..(r.end + typed_since).max(caret),
                    e.new_text.clone(),
                )
            }
            None => (
                c.start..caret,
                item.insert_text
                    .clone()
                    .unwrap_or_else(|| item.label.clone()),
            ),
        };
        let range = range.start.min(caret)..range.end;
        let snippet = if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
            text::expand_snippet(&insert)
        } else {
            text::Snippet {
                text: insert,
                selection: None,
            }
        };
        // Extra edits, such as an import at the top of the file.
        let extra = item
            .additional_text_edits
            .as_deref()
            .map(|e| text::char_edits(doc.text(), e, c.encoding))
            .unwrap_or_default();
        let start = text::map_offset(range.start, &extra);
        let len = snippet.text.chars().count();
        let selection = match snippet.selection {
            Some(s) => Selection::new(start + s.start, start + s.end),
            None => Selection::caret(start + len),
        };
        let mut edits = extra;
        edits.push(CharEdit {
            range,
            text: snippet.text,
        });
        self.documents[self.active]
            .apply_edits(edits.iter().map(|e| (e.range.clone(), e.text.as_str())));
        self.set_selection(ctx, selection, true);
    }

    fn show_completion(&mut self, ctx: &egui::Context, caret: Rect, colors: &UiColors) {
        let Some(c) = &mut self.language.completion else {
            return;
        };
        let mut clicked = None;
        let row_height = ctx.global_style().text_styles[&TextStyle::Monospace].size + 8.0;
        let below = caret.bottom() + COMPLETION_ROWS as f32 * row_height + 80.0
            < ctx.content_rect().bottom();
        let (pos, pivot) = if below {
            (caret.left_bottom() + vec2(0.0, 2.0), Align2::LEFT_TOP)
        } else {
            (caret.left_top() - vec2(0.0, 2.0), Align2::LEFT_BOTTOM)
        };
        egui::Area::new(egui::Id::unique("lsp_completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .pivot(pivot)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        ui.set_width(460.0);
                        egui::ScrollArea::vertical()
                            .max_height(row_height * COMPLETION_ROWS as f32)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for (row, &i) in c.shown.iter().enumerate() {
                                    let selected = row == c.selected;
                                    let r = completion_row(
                                        ui,
                                        &c.items[i],
                                        selected,
                                        row_height,
                                        colors,
                                    );
                                    if selected && c.scroll {
                                        r.scroll_to_me(None);
                                    }
                                    if r.clicked() {
                                        clicked = Some(row);
                                    }
                                }
                            });
                        if let Some(&i) = c.shown.get(c.selected) {
                            completion_docs(ui, &c.items[i]);
                        }
                    });
            });
        c.scroll = false;
        if let Some(row) = clicked {
            c.selected = row;
            self.accept_completion(ctx);
        }
    }

    // ---- rename ----------------------------------------------------------

    pub(super) fn open_rename(&mut self, ctx: &egui::Context) {
        self.active_pane = None;
        let Some((path, at)) = self.active_file(ctx) else {
            return;
        };
        let doc = &self.documents[self.active];
        let Some(word) = text::word_at(doc.text(), at) else {
            self.status = "Put the cursor on a symbol to rename it".to_owned();
            return;
        };
        if !self.lsp.supports(&path, Feature::Rename) {
            let text = doc.text().to_owned();
            // Reports why (no server, still starting, not supported).
            if let Err(e) = self.lsp.rename(&path, &text, at, "") {
                self.status = e;
            }
            return;
        }
        let input = doc
            .text()
            .chars()
            .skip(word.start)
            .take(word.len())
            .collect();
        self.language.rename = Some(RenameDialog {
            path,
            at: word.start,
            input,
            select_all: true,
        });
    }

    pub(super) fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.language.rename else {
            return;
        };
        let (enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let id = egui::Id::unique("rename_symbol");
        let input_id = id.with("input");
        let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, [0.0, 56.0]);
        let modal = egui::Modal::new(id)
            .area(area)
            .backdrop_color(Color32::from_black_alpha(60))
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label("Rename symbol");
                ui.add(
                    egui::TextEdit::singleline(&mut dialog.input)
                        .id(input_id)
                        .desired_width(f32::INFINITY),
                )
                .request_focus();
                ui.weak("Enter to rename, Escape to cancel");
            });
        if dialog.select_all {
            dialog.select_all = false;
            if let Some(mut state) = egui::TextEdit::load_state(ctx, input_id) {
                let all =
                    CCursorRange::two(CCursor::new(0), CCursor::new(dialog.input.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ctx, input_id);
            }
        }
        if escape || modal.backdrop_response.clicked() {
            self.language.rename = None;
            return;
        }
        if !enter {
            return;
        }
        let Some(dialog) = self.language.rename.take() else {
            return;
        };
        let new_name = dialog.input.trim();
        if new_name.is_empty() {
            return;
        }
        let Some(idx) = self.doc_index(&dialog.path) else {
            return;
        };
        let text = self.documents[idx].text().to_owned();
        match self.lsp.rename(&dialog.path, &text, dialog.at, new_name) {
            Ok(()) => self.status = format!("Renaming to {new_name}…"),
            Err(e) => self.status = e,
        }
        if let Some(id) = self.active_editor_id() {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    /// Applies a rename (or a server-requested edit) to open buffers, and
    /// writes files that aren't open directly.
    fn apply_workspace_edit(
        &mut self,
        ctx: &egui::Context,
        label: &str,
        edit: lsp_types::WorkspaceEdit,
        encoding: Encoding,
    ) -> Result<String, String> {
        let (files, skipped) = workspace_edit_files(edit);
        let mut changes = 0;
        let mut errors = Vec::new();
        let file_count = files.len();
        for (path, edits) in files {
            changes += edits.len();
            if let Some(idx) = self.doc_index(&path) {
                let char_edits = text::char_edits(self.documents[idx].text(), &edits, encoding);
                self.edit_document(ctx, idx, &char_edits);
                continue;
            }
            let result = std::fs::read_to_string(&path).and_then(|old| {
                let new = text::apply_edits(&old, &text::char_edits(&old, &edits, encoding));
                std::fs::write(&path, new)
            });
            if let Err(e) = result {
                errors.push(format!("{}: {e}", self.display_path(&path)));
            }
        }
        if file_count > 0 {
            self.source_control.refresh();
        }
        if !errors.is_empty() {
            return Err(format!("{label} failed for {}", errors.join(", ")));
        }
        if changes == 0 {
            return Ok(format!("{label}: nothing to change"));
        }
        let mut msg = format!(
            "{label}: {changes} change{} in {file_count} file{}",
            if changes == 1 { "" } else { "s" },
            if file_count == 1 { "" } else { "s" }
        );
        if skipped > 0 {
            msg.push_str(&format!(" ({skipped} file operations skipped)"));
        }
        Ok(msg)
    }

    // ---- formatting ------------------------------------------------------

    /// Asks the server to format the active file. With `then_save`, the file
    /// is saved once the edits arrive. Returns false if no request was sent.
    pub(super) fn format_document(&mut self, then_save: bool) -> bool {
        self.active_pane = None;
        let Some(doc) = self.documents.get(self.active) else {
            return false;
        };
        let Some(path) = doc.path.clone() else {
            return false;
        };
        let text = doc.text().to_owned();
        let indent = ops::indent_unit(&text);
        match self.lsp.format(&path, &text, &indent, then_save) {
            Ok(()) => {
                self.language.formatting = Some(FormatRequest {
                    path,
                    text,
                    then_save,
                    started: Instant::now(),
                });
                true
            }
            Err(e) => {
                if !then_save {
                    self.status = e;
                }
                false
            }
        }
    }

    /// Save, formatting first when format-on-save is on and the server can.
    pub(super) fn save_active(&mut self) {
        let formats = self.settings.format_on_save
            && self
                .documents
                .get(self.active)
                .and_then(|d| d.path.as_deref())
                .is_some_and(|p| self.lsp.supports(p, Feature::Formatting));
        if formats && self.format_document(true) {
            return;
        }
        self.save(self.active);
    }

    fn on_format(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
        edits: &[lsp_types::TextEdit],
        encoding: Encoding,
        then_save: bool,
    ) {
        let Some(req) = self
            .language
            .formatting
            .take_if(|r| r.path == path && r.then_save == then_save)
        else {
            return;
        };
        let Some(idx) = self.doc_index(path) else {
            return;
        };
        if self.documents[idx].text() == req.text {
            let char_edits = text::char_edits(&req.text, edits, encoding);
            self.edit_document(ctx, idx, &char_edits);
            if !then_save {
                self.status = if edits.is_empty() {
                    "Already formatted".to_owned()
                } else {
                    "Formatted".to_owned()
                };
            }
        } else if !edits.is_empty() {
            self.status = "The file changed while formatting; not formatted".to_owned();
        }
        if then_save {
            self.save(idx);
        }
    }

    // ---- status bar ------------------------------------------------------

    /// Problem counts and the active file's server, for the status bar
    /// (laid out right to left).
    pub(super) fn language_status(&mut self, ui: &mut egui::Ui) {
        let colors = self.current_theme().ui.clone();
        let activity = self
            .documents
            .get(self.active)
            .and_then(|d| d.path.as_deref())
            .and_then(|p| self.lsp.activity(p));
        if let Some((name, activity)) = activity {
            match activity {
                ServerActivity::Ready => {
                    ui.weak(name).on_hover_text("Language server running");
                }
                ServerActivity::Starting => {
                    ui.weak(format!("{name} starting…"));
                    ui.spinner();
                }
                ServerActivity::Busy(what) => {
                    ui.add(egui::Label::new(egui::RichText::new(&what).weak()).truncate())
                        .on_hover_text(format!("{name}: {what}"));
                    ui.spinner();
                }
                ServerActivity::Stopped(reason) => {
                    if ui
                        .link(
                            egui::RichText::new(format!("{name} stopped")).color(colors.warning.0),
                        )
                        .on_hover_text(format!("{reason}\n\nClick to restart language servers"))
                        .clicked()
                    {
                        self.restart_language_servers();
                    }
                }
            }
            ui.separator();
        }
        let (errors, warnings) = self.lsp.counts();
        let text_color = ui.visuals().text_color();
        let mut job = LayoutJob::default();
        let font = TextStyle::Body.resolve(ui.style());
        let error_color = if errors > 0 {
            colors.error.0
        } else {
            text_color
        };
        let warning_color = if warnings > 0 {
            colors.warning.0
        } else {
            text_color
        };
        job.append("⊗ ", 0.0, TextFormat::simple(font.clone(), error_color));
        job.append(
            &errors.to_string(),
            0.0,
            TextFormat::simple(font.clone(), text_color),
        );
        job.append("  ⚠ ", 0.0, TextFormat::simple(font.clone(), warning_color));
        job.append(
            &warnings.to_string(),
            0.0,
            TextFormat::simple(font, text_color),
        );
        let shortcut = crate::commands::Command::ShowProblems
            .shortcuts()
            .first()
            .map(|s| format!(" ({})", ui.ctx().format_shortcut(s)))
            .unwrap_or_default();
        if ui
            .add(egui::Button::new(job).frame(false))
            .on_hover_text(format!("Show problems{shortcut}"))
            .clicked()
        {
            self.show_problems();
        }
    }
}

impl CompletionPopup {
    /// Filters and ranks the items for the typed `query`.
    fn filter(&mut self, query: &str) {
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let key = item.filter_text.as_deref().unwrap_or(&item.label);
                fuzzy::score(query, key).map(|m| (m.score, i))
            })
            .collect();
        let sort_key = |i: usize| {
            let item = &self.items[i];
            item.sort_text.as_deref().unwrap_or(&item.label)
        };
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| sort_key(a.1).cmp(sort_key(b.1))));
        self.shown = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = if query.is_empty() {
            self.shown
                .iter()
                .position(|&i| self.items[i].preselect == Some(true))
                .unwrap_or(0)
        } else {
            0
        };
        self.query = query.to_owned();
        self.scroll = true;
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Error => "Error",
        Severity::Warning => "Warning",
        Severity::Info => "Info",
        Severity::Hint => "Hint",
    }
}

fn severity_color(s: Severity, colors: &UiColors) -> Color32 {
    match s {
        Severity::Error => colors.error.0,
        Severity::Warning => colors.warning.0,
        Severity::Info => colors.accent.0,
        Severity::Hint => colors.text_muted.0,
    }
}

fn problem_row(ui: &mut egui::Ui, p: &Problem, colors: &UiColors) {
    ui.horizontal_wrapped(|ui| {
        let color = severity_color(p.severity, colors);
        let mark = match p.severity {
            Severity::Error => "⊗",
            Severity::Warning => "⚠",
            Severity::Info | Severity::Hint => "ℹ",
        };
        ui.colored_label(color, mark);
        ui.label(&p.message);
        if let Some(source) = &p.source {
            ui.weak(source);
        }
    });
}

/// Paints diagnostics under the text: wavy lines for errors, warnings and
/// info, a short dotted line for hints.
pub(super) fn paint_problems(
    ui: &egui::Ui,
    output: &TextEditOutput,
    problems: &[Problem],
    visible: &Range<usize>,
    colors: &UiColors,
) {
    let offset = output.galley_pos.to_vec2();
    let galley = &output.galley;
    let mut shapes = Vec::new();
    // Least severe first, so errors are drawn on top.
    for p in problems.iter().rev() {
        if p.range.end < visible.start || p.range.start > visible.end {
            continue;
        }
        let color = severity_color(p.severity, colors);
        let range = if p.range.is_empty() {
            p.range.start..p.range.start + 1
        } else {
            p.range.clone()
        };
        for r in super::range_rects(galley, range) {
            let r = r.translate(offset);
            let y = r.bottom() - 1.0;
            if p.severity == Severity::Hint {
                let end = r.left() + (r.width()).min(12.0);
                let mut x = r.left();
                while x < end {
                    shapes.push(Shape::circle_filled(pos2(x, y), 0.8, color));
                    x += 3.0;
                }
                continue;
            }
            let mut points = Vec::new();
            let mut x = r.left();
            let mut up = true;
            while x <= r.right() {
                points.push(pos2(x, if up { y - 1.5 } else { y + 0.5 }));
                x += 2.0;
                up = !up;
            }
            if points.len() >= 2 {
                shapes.push(Shape::line(points, Stroke::new(1.0, color)));
            }
        }
    }
    ui.painter().extend(shapes);
}

/// The char under `pos` (screen coordinates) and its screen rect, if the
/// pointer is over a character rather than empty space.
pub(super) fn char_at(output: &TextEditOutput, pos: Pos2) -> Option<(usize, Rect)> {
    let offset = output.galley_pos.to_vec2();
    let local = pos - offset;
    let galley = &output.galley;
    let nearest: usize = galley.cursor_from_pos(local.to_vec2()).index.into();
    [nearest.saturating_sub(1), nearest]
        .into_iter()
        .find_map(|c| {
            let a = galley.pos_from_cursor(CCursor::new(c));
            let b = galley.pos_from_cursor(CCursor::new(c + 1));
            if (a.top() - b.top()).abs() > 0.5 || b.left() <= a.left() {
                return None;
            }
            let r = Rect::from_min_max(a.min, pos2(b.left(), a.bottom()));
            r.contains(local).then(|| (c, r.translate(offset)))
        })
}

/// Underlines `range` like a link (for Cmd/Ctrl+hover over a symbol).
pub(super) fn underline(
    ui: &egui::Ui,
    output: &TextEditOutput,
    range: Range<usize>,
    color: Color32,
) {
    let offset = output.galley_pos.to_vec2();
    for r in super::range_rects(&output.galley, range) {
        let r = r.translate(offset);
        ui.painter().line_segment(
            [
                pos2(r.left(), r.bottom() - 1.0),
                pos2(r.right(), r.bottom() - 1.0),
            ],
            Stroke::new(1.0, color),
        );
    }
}

/// A short tag and color slot for a completion kind.
fn kind_tag(kind: Option<CompletionItemKind>) -> (&'static str, u8) {
    use CompletionItemKind as K;
    match kind {
        Some(K::FUNCTION | K::METHOD | K::CONSTRUCTOR) => ("fn", 0),
        Some(K::VARIABLE | K::FIELD | K::PROPERTY) => ("var", 1),
        Some(K::CLASS | K::STRUCT | K::INTERFACE | K::ENUM | K::TYPE_PARAMETER) => ("type", 2),
        Some(K::MODULE) => ("mod", 2),
        Some(K::CONSTANT | K::ENUM_MEMBER | K::VALUE) => ("const", 1),
        Some(K::KEYWORD) => ("kw", 3),
        Some(K::SNIPPET) => ("snip", 3),
        Some(K::FILE | K::FOLDER) => ("file", 3),
        _ => ("", 3),
    }
}

fn completion_row(
    ui: &mut egui::Ui,
    item: &CompletionItem,
    selected: bool,
    height: f32,
    colors: &UiColors,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let visuals = ui.visuals();
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 3.0, visuals.selection.bg_fill);
    } else if response.hovered() {
        painter.rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let mono = TextStyle::Monospace.resolve(ui.style());
    let small = TextStyle::Small.resolve(ui.style());
    let inner = rect.shrink2(vec2(6.0, 0.0));

    let (tag, slot) = kind_tag(item.kind);
    let tag_color = match slot {
        0 => colors.accent.0,
        1 => colors.modified.0,
        2 => colors.added.0,
        _ => colors.text_muted.0,
    };
    let tag_width = 38.0;
    painter.text(
        pos2(inner.left(), inner.center().y),
        Align2::LEFT_CENTER,
        tag,
        small.clone(),
        tag_color,
    );

    let deprecated = item.deprecated == Some(true)
        || item
            .tags
            .as_ref()
            .is_some_and(|t| t.contains(&lsp_types::CompletionItemTag::DEPRECATED));
    let text_color = if selected {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    let detail = item
        .label_details
        .as_ref()
        .and_then(|d| d.description.clone().or_else(|| d.detail.clone()))
        .or_else(|| item.detail.clone())
        .unwrap_or_default();
    let detail = detail.lines().next().unwrap_or_default().to_owned();
    let detail_galley = painter.layout(
        detail,
        small,
        colors.text_muted.0,
        (inner.width() * 0.45).max(40.0),
    );
    let detail_width = detail_galley.size().x;
    // Only the first row of a wrapped detail is useful; clip the rest.
    let detail_pos = pos2(
        inner.right() - detail_width,
        inner.center().y - detail_galley.rows.first().map_or(0.0, |r| r.height()) / 2.0,
    );
    painter
        .with_clip_rect(rect)
        .galley(detail_pos, detail_galley, colors.text_muted.0);

    let mut job = LayoutJob::single_section(item.label.clone(), {
        let mut f = TextFormat::simple(mono, text_color);
        if deprecated {
            f.strikethrough = Stroke::new(1.0, text_color);
        }
        f
    });
    job.wrap = egui::text::TextWrapping::truncate_at_width(
        (inner.width() - tag_width - detail_width - 12.0).max(60.0),
    );
    let label = painter.layout_job(job);
    painter.galley(
        pos2(
            inner.left() + tag_width,
            inner.center().y - label.size().y / 2.0,
        ),
        label,
        text_color,
    );
    response
}

/// Detail and documentation of the selected completion, under the list.
fn completion_docs(ui: &mut egui::Ui, item: &CompletionItem) {
    let docs = match &item.documentation {
        Some(Documentation::String(s)) => s.clone(),
        Some(Documentation::MarkupContent(m)) => m.value.clone(),
        None => String::new(),
    };
    let detail = item.detail.clone().unwrap_or_default();
    if docs.trim().is_empty() && detail.trim().is_empty() {
        return;
    }
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("completion_docs")
        .max_height(160.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if !detail.trim().is_empty() {
                ui.label(egui::RichText::new(detail).monospace().small());
            }
            if !docs.trim().is_empty() {
                markdown::show(ui, &docs);
            }
        });
}

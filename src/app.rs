//! Top-level application state and UI layout.

use std::ops::Range;
use std::path::{Path, PathBuf};

use egui::text::{CCursor, CCursorRange};
use egui::{Key, KeyboardShortcut, Modifiers, TextStyle};

use crate::editor::{Document, Highlighter, gutter, search};
use crate::find_bar::{FindAction, FindBar, GotoLine};
use crate::workspace::{FileNode, Workspace};

const SHORTCUT_NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SHORTCUT_OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SHORTCUT_SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SHORTCUT_CLOSE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);
const SHORTCUT_UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
const SHORTCUT_REDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
const SHORTCUT_REDO_ALT: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
const SHORTCUT_FIND: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const SHORTCUT_REPLACE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::H);
const SHORTCUT_GOTO_LINE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::G);
const SHORTCUT_FIND_NEXT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F3);
const SHORTCUT_FIND_PREV: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::F3);

/// Something waiting on the user to decide what to do with unsaved changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseRequest {
    /// Close the tab holding the document with this id.
    Tab(egui::Id),
    /// Close the whole window.
    Window,
}

enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// A selection to apply to the active editor on the next frame.
struct PendingSelection {
    range: Range<usize>,
    /// Move keyboard focus to the editor (false while typing in the find bar).
    focus: bool,
}

pub struct ForgeApp {
    workspace: Option<Workspace>,
    documents: Vec<Document>,
    active: usize,
    highlighter: Highlighter,
    status: String,
    /// 1-based (line, column) of the cursor in the active document.
    cursor: (usize, usize),
    /// Current selection in the active document, as a char range (empty = caret).
    selection: Range<usize>,
    /// Selection to apply (and scroll to) in the active editor on the next frame.
    pending_selection: Option<PendingSelection>,
    find: FindBar,
    goto_line: GotoLine,
    /// Close waiting on the unsaved-changes prompt.
    pending_close: Option<CloseRequest>,
    /// Set once the user has resolved unsaved changes, so the next close request goes through.
    allow_window_close: bool,
}

impl ForgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, root: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Self {
            workspace: root.filter(|p| p.is_dir()).map(Workspace::open),
            documents: vec![Document::untitled()],
            active: 0,
            highlighter: Highlighter::default(),
            status: "Ready".to_owned(),
            cursor: (1, 1),
            selection: 0..0,
            pending_selection: None,
            find: FindBar::default(),
            goto_line: GotoLine::default(),
            pending_close: None,
            allow_window_close: false,
        }
    }

    // ---- actions -------------------------------------------------------

    fn new_file(&mut self) {
        self.documents.push(Document::untitled());
        self.active = self.documents.len() - 1;
    }

    fn open_file(&mut self, path: &Path) {
        if let Some(idx) = self
            .documents
            .iter()
            .position(|d| d.path.as_deref() == Some(path))
        {
            self.active = idx;
            return;
        }
        match Document::open(path) {
            Ok(doc) => {
                self.documents.push(doc);
                self.active = self.documents.len() - 1;
                self.status = format!("Opened {}", path.display());
            }
            Err(e) => self.status = format!("Error: {e:#}"),
        }
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.open_file(&path);
        }
    }

    fn open_folder_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.status = format!("Opened folder {}", path.display());
            self.workspace = Some(Workspace::open(path));
        }
    }

    fn save_active(&mut self) {
        self.save_document(self.active);
    }

    /// Saves a document, asking for a path if it has none. Returns true on success.
    fn save_document(&mut self, idx: usize) -> bool {
        let Some(doc) = self.documents.get_mut(idx) else {
            return false;
        };
        let result = if doc.path.is_some() {
            doc.save()
        } else if let Some(path) = rfd::FileDialog::new()
            .set_file_name(doc.title())
            .save_file()
        {
            doc.save_as(path)
        } else {
            return false;
        };
        match result {
            Ok(()) => {
                self.status = format!("Saved {}", doc.title());
                true
            }
            Err(e) => {
                self.status = format!("Error: {e:#}");
                false
            }
        }
    }

    fn undo(&mut self) {
        if let Some(doc) = self.documents.get_mut(self.active) {
            if let Some(cursor) = doc.buffer.undo() {
                self.select(cursor..cursor, true);
            } else {
                self.status = "Nothing to undo".to_owned();
            }
        }
    }

    fn redo(&mut self) {
        if let Some(doc) = self.documents.get_mut(self.active) {
            if let Some(cursor) = doc.buffer.redo() {
                self.select(cursor..cursor, true);
            } else {
                self.status = "Nothing to redo".to_owned();
            }
        }
    }

    fn select(&mut self, range: Range<usize>, focus: bool) {
        self.pending_selection = Some(PendingSelection { range, focus });
    }

    fn open_find(&mut self, replace: bool) {
        let seed = self.documents.get(self.active).and_then(|doc| {
            let r = self.selection.clone();
            (!r.is_empty()).then(|| {
                egui::TextBuffer::char_range(&doc.buffer, r.start.into()..r.end.into()).to_owned()
            })
        });
        self.find.show(replace, seed);
    }

    /// Moves to the next/previous match relative to the cursor.
    fn find_step(&mut self, forward: bool) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if !self.find.open {
            self.open_find(false);
            return;
        }
        self.find.refresh(doc);
        let matches = self.find.matches();
        let idx = if forward {
            search::next_match(matches, self.selection.end.max(self.selection.start + 1))
        } else {
            search::prev_match(matches, self.selection.start)
        };
        // Starting right on a match (e.g. just opened with a selection) should not skip it.
        let idx = match (forward, self.find.current) {
            (true, None) => search::next_match(matches, self.selection.start),
            _ => idx,
        };
        if let Some(i) = idx {
            let range = matches[i].clone();
            self.find.current = Some(i);
            self.select(range, false);
        }
    }

    fn replace_current(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        self.find.refresh(doc);
        // Only replace when the current match is what's selected; otherwise just find it.
        let current = self
            .find
            .current
            .filter(|&i| self.find.matches().get(i) == Some(&self.selection));
        let Some(i) = current else {
            self.find_step(true);
            return;
        };
        let edits = self.find.replacements(doc, [i]);
        let placed = doc.buffer.replace_ranges(edits);
        if let Some(r) = placed.first() {
            self.selection = r.end..r.end;
            self.find.current = None;
            self.find.refresh(doc);
            self.find_step(true);
        }
    }

    fn replace_all(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        self.find.refresh(doc);
        let edits = self.find.replacements(doc, 0..self.find.matches().len());
        let count = edits.len();
        if count > 0 {
            doc.buffer.replace_ranges(edits);
        }
        self.status = format!(
            "Replaced {count} occurrence{}",
            if count == 1 { "" } else { "s" }
        );
    }

    fn handle_find_action(&mut self, action: FindAction) {
        match action {
            FindAction::Next => self.find_step(true),
            FindAction::Prev => self.find_step(false),
            FindAction::Replace => self.replace_current(),
            FindAction::ReplaceAll => self.replace_all(),
            FindAction::Close => {
                self.find.close();
                let sel = self.selection.clone();
                self.select(sel, true);
            }
        }
    }

    fn goto(&mut self, line: usize, col: usize) {
        if let Some(doc) = self.documents.get(self.active) {
            let at = doc.buffer.char_at_line_col(line, col);
            self.select(at..at, true);
        }
    }

    /// Whether keyboard input currently targets the active editor (or nothing),
    /// as opposed to some other text field with its own undo.
    fn editor_has_keyboard(&self, ctx: &egui::Context) -> bool {
        let focused = ctx.memory(|m| m.focused());
        focused.is_none() || focused == self.documents.get(self.active).map(|d| d.id)
    }

    /// Closes a tab, first asking what to do if it has unsaved changes.
    fn request_close_tab(&mut self, idx: usize) {
        match self.documents.get(idx) {
            Some(doc) if doc.is_dirty() => {
                self.active = idx;
                self.pending_close = Some(CloseRequest::Tab(doc.id));
            }
            Some(_) => self.close_tab(idx),
            None => {}
        }
    }

    /// Closes a tab unconditionally, discarding unsaved changes.
    fn close_tab(&mut self, idx: usize) {
        if idx < self.documents.len() {
            self.documents.remove(idx);
        }
        if self.documents.is_empty() {
            self.documents.push(Document::untitled());
        }
        if self.active >= self.documents.len() || self.active > idx {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (new, open, save, close) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&SHORTCUT_NEW),
                i.consume_shortcut(&SHORTCUT_OPEN),
                i.consume_shortcut(&SHORTCUT_SAVE),
                i.consume_shortcut(&SHORTCUT_CLOSE),
            )
        });
        if new {
            self.new_file();
        }
        if open {
            self.open_file_dialog();
        }
        if save {
            self.save_active();
        }
        if close {
            self.request_close_tab(self.active);
        }

        let (find, replace, goto, find_prev, find_next) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&SHORTCUT_FIND),
                i.consume_shortcut(&SHORTCUT_REPLACE),
                i.consume_shortcut(&SHORTCUT_GOTO_LINE),
                // Shift+F3 first: the plain F3 shortcut would also match it.
                i.consume_shortcut(&SHORTCUT_FIND_PREV),
                i.consume_shortcut(&SHORTCUT_FIND_NEXT),
            )
        });
        if find || replace {
            self.open_find(replace);
        }
        if goto {
            self.goto_line.show();
        }
        if find_next {
            self.find_step(true);
        }
        if find_prev {
            self.find_step(false);
        }

        if self.editor_has_keyboard(ctx) {
            // Consumed before the TextEdit runs, so egui's built-in undo never sees them.
            // Redo first: the plain Ctrl+Z shortcut would also match Ctrl+Shift+Z.
            let (redo, undo) = ctx.input_mut(|i| {
                let redo =
                    i.consume_shortcut(&SHORTCUT_REDO_ALT) | i.consume_shortcut(&SHORTCUT_REDO);
                (redo, i.consume_shortcut(&SHORTCUT_UNDO))
            });
            if redo {
                self.redo();
            }
            if undo {
                self.undo();
            }
        }
    }

    // ---- panels --------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                let ctx = ui.ctx().clone();
                if ui
                    .add(
                        egui::Button::new("New File")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_NEW)),
                    )
                    .clicked()
                {
                    self.new_file();
                }
                if ui
                    .add(
                        egui::Button::new("Open File…")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_OPEN)),
                    )
                    .clicked()
                {
                    self.open_file_dialog();
                }
                if ui.button("Open Folder…").clicked() {
                    self.open_folder_dialog();
                }
                ui.separator();
                if ui
                    .add(
                        egui::Button::new("Save")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_SAVE)),
                    )
                    .clicked()
                {
                    self.save_active();
                }
                if ui
                    .add(
                        egui::Button::new("Close Tab")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_CLOSE)),
                    )
                    .clicked()
                {
                    self.request_close_tab(self.active);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let ctx = ui.ctx().clone();
                let (can_undo, can_redo) =
                    self.documents.get(self.active).map_or((false, false), |d| {
                        (d.buffer.can_undo(), d.buffer.can_redo())
                    });
                if ui
                    .add_enabled(
                        can_undo,
                        egui::Button::new("Undo")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_UNDO)),
                    )
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(
                        can_redo,
                        egui::Button::new("Redo")
                            .shortcut_text(ctx.format_shortcut(&SHORTCUT_REDO)),
                    )
                    .clicked()
                {
                    self.redo();
                }
                ui.separator();
                if menu_item(ui, "Find…", &SHORTCUT_FIND) {
                    self.open_find(false);
                }
                if menu_item(ui, "Replace…", &SHORTCUT_REPLACE) {
                    self.open_find(true);
                }
                if menu_item(ui, "Find Next", &SHORTCUT_FIND_NEXT) {
                    self.find_step(true);
                }
                if menu_item(ui, "Find Previous", &SHORTCUT_FIND_PREV) {
                    self.find_step(false);
                }
                ui.separator();
                if menu_item(ui, "Go to Line…", &SHORTCUT_GOTO_LINE) {
                    self.goto_line.show();
                }
            });
        });
    }

    fn explorer(&mut self, ui: &mut egui::Ui) {
        ui.heading("Explorer");
        ui.separator();
        let Some(ws) = &mut self.workspace else {
            if ui.button("Open Folder…").clicked() {
                self.open_folder_dialog();
            }
            return;
        };
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| file_tree(ui, &mut ws.root, true, &mut clicked));
        if let Some(path) = clicked {
            self.open_file(&path);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(doc) = self.documents.get(self.active) {
                    ui.label(doc.extension());
                    ui.separator();
                    ui.label(format!("Ln {}, Col {}", self.cursor.0, self.cursor.1));
                }
            });
        });
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut to_close = None;
        egui::ScrollArea::horizontal()
            .id_salt("tab_bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (idx, doc) in self.documents.iter().enumerate() {
                        let label = if doc.is_dirty() {
                            format!("● {}", doc.title())
                        } else {
                            doc.title()
                        };
                        if ui.selectable_label(idx == self.active, label).clicked() {
                            self.active = idx;
                        }
                        if ui.small_button("×").clicked() {
                            to_close = Some(idx);
                        }
                        ui.separator();
                    }
                });
            });
        if let Some(idx) = to_close {
            self.request_close_tab(idx);
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let extension = doc.extension().to_owned();
        let highlighter = &mut self.highlighter;
        let font = TextStyle::Monospace.resolve(ui.style());
        let digit_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let gutter_width = gutter::width(doc.line_count(), digit_width);
        let current_line = self.cursor.0;

        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = highlighter.highlight(text.as_str(), &extension, font.clone());
            job.wrap.max_width = wrap_width;
            ui.painter().layout_job(job)
        };

        // Apply a programmatic selection (undo/redo, find, go to line) before rendering.
        let reveal = self.pending_selection.take().map(|pending| {
            let range = pending.range;
            let mut state = egui::TextEdit::load_state(ui.ctx(), doc.id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(range.start),
                CCursor::new(range.end),
            )));
            egui::TextEdit::store_state(ui.ctx(), doc.id, state);
            if pending.focus {
                ui.memory_mut(|m| m.request_focus(doc.id));
            }
            CCursor::new(range.end)
        });
        let find = &self.find;
        let matches = if find.open { find.matches() } else { &[] };

        let mut output = egui::ScrollArea::both()
            .id_salt("editor_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                let output = egui::TextEdit::multiline(&mut doc.buffer)
                    .id(doc.id)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(40)
                    .margin(egui::Margin {
                        left: gutter_width as i8,
                        right: 4,
                        top: 2,
                        bottom: 2,
                    })
                    .layouter(&mut layouter)
                    .show(ui);
                gutter::paint(ui, &output, current_line, &font);
                paint_matches(ui, &output, matches, find.current);
                if let Some(cursor) = reveal {
                    let rect = output
                        .galley
                        .pos_from_cursor(cursor)
                        .translate(output.galley_pos.to_vec2());
                    ui.scroll_to_rect(rect.expand(8.0), Some(egui::Align::Center));
                }
                output
            })
            .inner;

        // Undo history lives in `Buffer`; drop egui's whole-text snapshots.
        output.state.clear_undoer();

        if let Some(range) = output.cursor_range {
            let [start, end] = range.sorted_cursors();
            self.selection = start.index.into()..end.index.into();
            self.cursor = doc.line_col(range.primary.index.into());
        }
    }

    /// Intercepts window close while there are unsaved changes.
    fn intercept_window_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_window_close {
            return;
        }
        if self.documents.iter().any(Document::is_dirty) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_close = Some(CloseRequest::Window);
        }
    }

    /// Shows the unsaved-changes prompt for a pending close, if any.
    fn close_prompt(&mut self, ctx: &egui::Context) {
        let Some(request) = self.pending_close else {
            return;
        };
        let dirty: Vec<String> = match request {
            CloseRequest::Tab(id) => self
                .documents
                .iter()
                .filter(|d| d.id == id && d.is_dirty())
                .map(Document::title)
                .collect(),
            CloseRequest::Window => self
                .documents
                .iter()
                .filter(|d| d.is_dirty())
                .map(Document::title)
                .collect(),
        };
        if dirty.is_empty() {
            // Saved or closed some other way in the meantime.
            self.pending_close = None;
            self.finish_close(ctx, request);
            return;
        }

        let modal = egui::Modal::new(egui::Id::unique("unsaved_changes")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            let (heading, save_label, discard_label) = match &dirty[..] {
                [one] => (
                    format!("Do you want to save the changes you made to {one}?"),
                    "Save",
                    "Don't Save",
                ),
                many => (
                    format!("Do you want to save changes to {} files?", many.len()),
                    "Save All",
                    "Discard All",
                ),
            };
            ui.label(egui::RichText::new(heading).strong());
            if dirty.len() > 1 {
                for title in &dirty {
                    ui.label(format!("• {title}"));
                }
            }
            ui.label("Your changes will be lost if you don't save them.");
            ui.add_space(8.0);
            let mut choice = None;
            ui.horizontal(|ui| {
                if ui.button(save_label).clicked() {
                    choice = Some(CloseChoice::Save);
                }
                if ui.button(discard_label).clicked() {
                    choice = Some(CloseChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(CloseChoice::Cancel);
                }
            });
            choice
        });
        let choice = if modal.should_close() {
            Some(CloseChoice::Cancel)
        } else {
            modal.inner
        };

        match choice {
            None => {}
            Some(CloseChoice::Cancel) => self.pending_close = None,
            Some(CloseChoice::Discard) => {
                self.pending_close = None;
                self.finish_close(ctx, request);
            }
            Some(CloseChoice::Save) => {
                self.pending_close = None;
                let targets: Vec<usize> = (0..self.documents.len())
                    .filter(|&i| {
                        let d = &self.documents[i];
                        d.is_dirty()
                            && (request == CloseRequest::Window
                                || CloseRequest::Tab(d.id) == request)
                    })
                    .collect();
                // Stop at the first failure (or cancelled save dialog) and keep everything open.
                if targets.into_iter().all(|i| self.save_document(i)) {
                    self.finish_close(ctx, request);
                }
            }
        }
    }

    fn finish_close(&mut self, ctx: &egui::Context, request: CloseRequest) {
        match request {
            CloseRequest::Tab(id) => {
                if let Some(idx) = self.documents.iter().position(|d| d.id == id) {
                    self.close_tab(idx);
                }
            }
            CloseRequest::Window => {
                self.allow_window_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Find bar above the editor; jumps to the first match as the query is typed.
    fn find_bar(&mut self, ui: &mut egui::Ui) {
        if !self.find.open {
            return;
        }
        if let Some(action) = self.find.ui(ui) {
            self.handle_find_action(action);
        }
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if self.find.refresh(doc)
            && let Some(i) = search::next_match(self.find.matches(), self.selection.start)
        {
            self.find.current = Some(i);
            let range = self.find.matches()[i].clone();
            self.select(range, false);
        }
        ui.separator();
    }
}

/// A menu button with a shortcut hint. Returns true when clicked.
fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &KeyboardShortcut) -> bool {
    let hint = ui.ctx().format_shortcut(shortcut);
    ui.add(egui::Button::new(label).shortcut_text(hint))
        .clicked()
}

/// Highlights search matches inside the visible part of the editor.
fn paint_matches(
    ui: &egui::Ui,
    output: &egui::text_edit::TextEditOutput,
    matches: &[Range<usize>],
    current: Option<usize>,
) {
    if matches.is_empty() {
        return;
    }
    let clip = ui.clip_rect();
    let galley_pos = output.galley_pos;
    let origin = galley_pos.to_vec2();
    let galley = &output.galley;
    let first = galley.cursor_from_pos(clip.min - galley_pos).index.0;
    let last = galley.cursor_from_pos(clip.max - galley_pos).index.0;
    let start = matches.partition_point(|m| m.end < first);
    let accent = ui.visuals().selection.bg_fill;
    for (i, m) in matches.iter().enumerate().skip(start) {
        if m.start > last {
            break;
        }
        let a = galley
            .pos_from_cursor(CCursor::new(m.start))
            .translate(origin);
        let b = galley
            .pos_from_cursor(CCursor::new(m.end))
            .translate(origin);
        // Multi-line matches are marked on their first line only.
        let right = if (a.top() - b.top()).abs() < 1.0 {
            b.left()
        } else {
            a.left() + 8.0
        };
        let rect = egui::Rect::from_min_max(a.left_top(), egui::pos2(right, a.bottom()));
        let alpha = if Some(i) == current { 160 } else { 70 };
        ui.painter()
            .rect_filled(rect, 2.0, accent.gamma_multiply_u8(alpha));
    }
}

/// Recursively renders a directory tree. Sets `clicked` when a file is selected.
fn file_tree(ui: &mut egui::Ui, node: &mut FileNode, is_root: bool, clicked: &mut Option<PathBuf>) {
    if node.is_dir {
        let header = egui::CollapsingHeader::new(&node.name)
            .id_salt(&node.path)
            .default_open(is_root)
            .show(ui, |ui| {
                for child in node.children() {
                    file_tree(ui, child, false, clicked);
                }
            });
        header.header_response.context_menu(|ui| {
            if ui.button("Refresh").clicked() {
                node.refresh();
                ui.close();
            }
        });
    } else if ui.selectable_label(false, &node.name).clicked() {
        *clicked = Some(node.path.clone());
    }
}

impl eframe::App for ForgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.intercept_window_close(&ctx);
        self.handle_shortcuts(&ctx);

        egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("explorer")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| self.explorer(ui));

        let bg = self.highlighter.background();
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).fill(bg))
            .show(ui, |ui| {
                self.tab_bar(ui);
                ui.separator();
                self.find_bar(ui);
                self.editor(ui);
            });

        let line_count = self
            .documents
            .get(self.active)
            .map_or(1, Document::line_count);
        if let Some((line, col)) = self.goto_line.ui(ui.ctx(), line_count) {
            self.goto(line, col);
        }
        self.close_prompt(&ctx);
    }
}

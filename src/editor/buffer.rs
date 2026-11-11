//! Rope-backed text buffer with an edit-based undo/redo history.
//!
//! The [`Rope`] is the source of truth for indexing (line/column lookups, line
//! counts, char ↔ byte conversion), which stays O(log n) regardless of file
//! size. egui's `TextEdit` requires the whole text as one contiguous `&str`, so a
//! `String` mirror is kept in sync for rendering.
//!
//! All mutations go through [`TextBuffer::insert_text`] and
//! [`TextBuffer::delete_char_range`], which record each change as an [`Edit`].
//! History therefore costs memory proportional to what was changed, not to the
//! size of the file.

use std::ops::Range;
use std::time::{Duration, Instant};

use egui::TextBuffer;
use egui::text::CharIndex;
use ropey::Rope;

/// Consecutive edits closer together than this may be merged into one undo step.
const GROUP_TIMEOUT: Duration = Duration::from_millis(1000);

/// Upper bound on undo steps kept in memory.
const MAX_UNDO_STEPS: usize = 1000;

/// A single replacement: `removed` was replaced by `inserted` at char offset `at`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
}

impl Edit {
    fn inserted_end(&self) -> usize {
        self.at + self.inserted.chars().count()
    }
}

/// A group of edits undone/redone together.
#[derive(Debug)]
struct Transaction {
    /// Unique per distinct buffer state, used for dirty tracking.
    id: u64,
    edits: Vec<Edit>,
    last_edit: Instant,
}

impl Transaction {
    /// Whether `next` continues this transaction (typing a word, holding backspace,
    /// typing over a selection) rather than starting a new undo step.
    fn accepts(&self, next: &Edit, now: Instant) -> bool {
        if now.duration_since(self.last_edit) > GROUP_TIMEOUT {
            return false;
        }
        let Some(last) = self.edits.last() else {
            return false;
        };
        // Start a new step at each line break so undo doesn't swallow whole paragraphs.
        if next.inserted.contains('\n') || last.inserted.contains('\n') {
            return false;
        }
        let typing = next.removed.is_empty() && next.at == last.inserted_end();
        let backspace =
            next.inserted.is_empty() && next.at + next.removed.chars().count() == last.at;
        let forward_delete = next.inserted.is_empty() && next.at == last.at;
        let replace_selection =
            last.inserted.is_empty() && next.removed.is_empty() && next.at == last.at;
        typing || backspace || forward_delete || replace_selection
    }
}

#[derive(Debug)]
struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_id: u64,
    /// Transaction id at the last save; 0 means "no transactions".
    saved_id: u64,
    /// When set, the next edit always starts a new transaction.
    sealed: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_id: 0,
            sealed: false,
        }
    }
}

impl History {
    fn current_id(&self) -> u64 {
        self.undo.last().map_or(0, |t| t.id)
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn record(&mut self, edit: Edit, now: Instant) {
        self.redo.clear();
        let id = self.fresh_id();
        match self.undo.last_mut() {
            Some(t) if !self.sealed && t.id != self.saved_id && t.accepts(&edit, now) => {
                t.edits.push(edit);
                t.last_edit = now;
                // The merged transaction now describes a different state.
                t.id = id;
            }
            _ => {
                self.undo.push(Transaction {
                    id,
                    edits: vec![edit],
                    last_edit: now,
                });
                if self.undo.len() > MAX_UNDO_STEPS {
                    self.undo.remove(0);
                }
            }
        }
        self.sealed = false;
    }
}

pub struct Buffer {
    rope: Rope,
    text: String,
    history: History,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::from("")
    }
}

impl From<&str> for Buffer {
    fn from(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            text: text.to_owned(),
            history: History::default(),
        }
    }
}

impl Buffer {
    /// Number of lines; an empty buffer, or one ending in `\n`, counts the final empty line.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// 1-based (line, column) of a char offset. Offsets past the end are clamped.
    pub fn line_col(&self, char_idx: usize) -> (usize, usize) {
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(char_idx);
        (line + 1, char_idx - self.rope.line_to_char(line) + 1)
    }

    /// Whether the buffer differs from its state at the last [`Self::mark_saved`].
    pub fn is_modified(&self) -> bool {
        self.history.current_id() != self.history.saved_id
    }

    pub fn mark_saved(&mut self) {
        self.history.saved_id = self.history.current_id();
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// Reverts the last undo step. Returns where the cursor should go.
    pub fn undo(&mut self) -> Option<usize> {
        let t = self.history.undo.pop()?;
        let mut cursor = 0;
        for edit in t.edits.iter().rev() {
            let end = edit.inserted_end();
            self.apply(edit.at..end, &edit.removed);
            cursor = edit.at + edit.removed.chars().count();
        }
        self.history.redo.push(t);
        self.history.sealed = true;
        Some(cursor)
    }

    /// Re-applies the last undone step. Returns where the cursor should go.
    pub fn redo(&mut self) -> Option<usize> {
        let t = self.history.redo.pop()?;
        let mut cursor = 0;
        for edit in &t.edits {
            let end = edit.at + edit.removed.chars().count();
            self.apply(edit.at..end, &edit.inserted);
            cursor = edit.inserted_end();
        }
        self.history.undo.push(t);
        self.history.sealed = true;
        Some(cursor)
    }

    fn record_and_apply(&mut self, range: Range<usize>, text: &str) {
        if range.is_empty() && text.is_empty() {
            return;
        }
        let removed = self.rope.slice(range.clone()).to_string();
        self.history.record(
            Edit {
                at: range.start,
                removed,
                inserted: text.to_owned(),
            },
            Instant::now(),
        );
        self.apply(range, text);
    }

    /// Applies a change to both the rope and the string mirror without recording it.
    fn apply(&mut self, range: Range<usize>, text: &str) {
        let start_byte = self.rope.char_to_byte(range.start);
        let end_byte = self.rope.char_to_byte(range.end);
        self.rope.remove(range.clone());
        self.rope.insert(range.start, text);
        self.text.replace_range(start_byte..end_byte, text);
    }
}

impl TextBuffer for Buffer {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    fn insert_text(&mut self, text: &str, char_index: CharIndex) -> usize {
        let at = char_index.0.min(self.rope.len_chars());
        self.record_and_apply(at..at, text);
        text.chars().count()
    }

    fn delete_char_range(&mut self, char_range: Range<CharIndex>) {
        let len = self.rope.len_chars();
        let range = char_range.start.0.min(len)..char_range.end.0.min(len);
        self.record_and_apply(range, "");
    }

    fn char_range(&self, char_range: Range<CharIndex>) -> &str {
        let start = self.rope.char_to_byte(char_range.start.0);
        let end = self.rope.char_to_byte(char_range.end.0);
        &self.text[start..end]
    }

    fn byte_index_from_char_index(&self, char_index: CharIndex) -> egui::text::ByteIndex {
        egui::text::ByteIndex(self.rope.char_to_byte(char_index.0))
    }

    fn char_index_from_byte_index(&self, byte_index: egui::text::ByteIndex) -> CharIndex {
        CharIndex(self.rope.byte_to_char(byte_index.0))
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_chars(buf: &mut Buffer, at: usize, text: &str) {
        for (i, c) in text.chars().enumerate() {
            buf.insert_text(&c.to_string(), CharIndex(at + i));
        }
    }

    fn assert_in_sync(buf: &Buffer) {
        assert_eq!(buf.rope.to_string(), buf.text);
    }

    #[test]
    fn edits_keep_rope_and_string_in_sync() {
        let mut buf = Buffer::from("héllo wörld");
        buf.insert_text("✨", CharIndex(5));
        buf.delete_char_range(CharIndex(0)..CharIndex(2));
        assert_eq!(buf.as_str(), "llo✨ wörld");
        assert_in_sync(&buf);
        assert_eq!(buf.char_range(CharIndex(3)..CharIndex(4)), "✨");
    }

    #[test]
    fn line_col_and_line_count() {
        let buf = Buffer::from("ab\ncd\nef");
        assert_eq!(buf.line_col(0), (1, 1));
        assert_eq!(buf.line_col(2), (1, 3));
        assert_eq!(buf.line_col(3), (2, 1));
        assert_eq!(buf.line_col(7), (3, 2));
        assert_eq!(buf.line_col(999), (3, 3));
        assert_eq!(buf.line_count(), 3);
    }

    #[test]
    fn typed_word_is_one_undo_step() {
        let mut buf = Buffer::default();
        type_chars(&mut buf, 0, "hello");
        assert_eq!(buf.undo(), Some(0));
        assert_eq!(buf.as_str(), "");
        assert_eq!(buf.redo(), Some(5));
        assert_eq!(buf.as_str(), "hello");
        assert_in_sync(&buf);
    }

    #[test]
    fn newline_starts_new_undo_step() {
        let mut buf = Buffer::default();
        type_chars(&mut buf, 0, "ab\ncd");
        buf.undo();
        assert_eq!(buf.as_str(), "ab\n");
        buf.undo();
        assert_eq!(buf.as_str(), "ab");
    }

    #[test]
    fn backspace_run_is_one_undo_step() {
        let mut buf = Buffer::from("hello");
        for end in (1..=5).rev() {
            buf.delete_char_range(CharIndex(end - 1)..CharIndex(end));
        }
        assert_eq!(buf.as_str(), "");
        assert_eq!(buf.undo(), Some(5));
        assert_eq!(buf.as_str(), "hello");
    }

    #[test]
    fn typing_over_selection_is_one_undo_step() {
        let mut buf = Buffer::from("hello world");
        buf.delete_char_range(CharIndex(6)..CharIndex(11));
        type_chars(&mut buf, 6, "there");
        assert_eq!(buf.as_str(), "hello there");
        buf.undo();
        assert_eq!(buf.as_str(), "hello world");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut buf = Buffer::default();
        buf.insert_text("a", CharIndex(0));
        buf.undo();
        assert!(buf.can_redo());
        buf.insert_text("b", CharIndex(0));
        assert!(!buf.can_redo());
        assert_eq!(buf.redo(), None);
    }

    #[test]
    fn modified_tracks_saved_state_through_undo() {
        let mut buf = Buffer::from("x");
        assert!(!buf.is_modified());
        type_chars(&mut buf, 1, "yz");
        assert!(buf.is_modified());
        buf.mark_saved();
        assert!(!buf.is_modified());
        // Typing right after a save must not merge into the saved step.
        type_chars(&mut buf, 3, "!");
        assert!(buf.is_modified());
        buf.undo();
        assert!(!buf.is_modified());
        buf.undo();
        assert!(buf.is_modified());
        assert_eq!(buf.as_str(), "x");
        buf.redo();
        assert!(!buf.is_modified());
    }
}

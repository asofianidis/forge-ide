//! Rope-backed text buffer with an edit-based undo/redo history.
//!
//! The [`Rope`] is the source of truth for indexing (line/column lookups, line
//! counts, char ↔ byte conversion), which stays O(log n) regardless of file
//! size. egui's `TextEdit` requires the whole text as one contiguous `&str`, so a
//! `String` mirror is kept in sync for rendering.
//!
//! All mutations — typing in the widget as well as programmatic edits such as
//! line commands, replace, or language-server edits — are recorded as
//! replacements ([`Edit`]). History therefore costs memory proportional to what
//! was changed, not to the size of the file.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::{Duration, Instant};

use egui::TextBuffer;
use egui::text::{ByteIndex, CharIndex};
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

    fn removed_end(&self) -> usize {
        self.at + self.removed.chars().count()
    }
}

/// A group of edits undone/redone together, in the order they were applied.
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
        let backspace = next.inserted.is_empty() && next.removed_end() == last.at;
        let forward_delete = next.inserted.is_empty() && next.at == last.at;
        let replace_selection =
            last.inserted.is_empty() && next.removed.is_empty() && next.at == last.at;
        typing || backspace || forward_delete || replace_selection
    }
}

#[derive(Debug)]
struct History {
    undo: VecDeque<Transaction>,
    redo: Vec<Transaction>,
    next_id: u64,
    /// Id of the state below the oldest undo step (0 for the initial text,
    /// or the id of the last step dropped by the cap).
    base_id: u64,
    /// State id at the last save.
    saved_id: u64,
    /// When set, the next edit always starts a new transaction.
    sealed: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            next_id: 1,
            base_id: 0,
            saved_id: 0,
            sealed: false,
        }
    }
}

impl History {
    fn current_id(&self) -> u64 {
        self.undo.back().map_or(self.base_id, |t| t.id)
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn push(&mut self, edits: Vec<Edit>, now: Instant) {
        let id = self.fresh_id();
        self.undo.push_back(Transaction {
            id,
            edits,
            last_edit: now,
        });
        if self.undo.len() > MAX_UNDO_STEPS
            && let Some(dropped) = self.undo.pop_front()
        {
            self.base_id = dropped.id;
        }
    }

    /// Records a typed edit, merging it into the last step where it continues it.
    fn record(&mut self, edit: Edit, now: Instant) {
        self.redo.clear();
        let id = self.fresh_id();
        match self.undo.back_mut() {
            Some(t) if !self.sealed && t.id != self.saved_id && t.accepts(&edit, now) => {
                t.edits.push(edit);
                t.last_edit = now;
                // The merged transaction now describes a different state.
                t.id = id;
            }
            _ => self.push(vec![edit], now),
        }
        self.sealed = false;
    }

    /// Records edits as one step of their own; the next edit starts another.
    fn record_step(&mut self, edits: Vec<Edit>, now: Instant) {
        self.redo.clear();
        self.push(edits, now);
        self.sealed = true;
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

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("len_chars", &self.rope.len_chars())
            .field("undo_steps", &self.history.undo.len())
            .field("redo_steps", &self.history.redo.len())
            .finish()
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

    /// Char offset of a 1-based (line, column), clamped to the line and the text.
    pub fn char_index(&self, line: usize, col: usize) -> usize {
        let line = line.max(1) - 1;
        if line >= self.rope.len_lines() {
            return self.rope.len_chars();
        }
        let start = self.rope.line_to_char(line);
        // Lines break only on `\n` (see Cargo.toml), so every line but the
        // last ends in exactly one.
        let len = if line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(line + 1) - start - 1
        } else {
            self.rope.len_chars() - start
        };
        start + col.saturating_sub(1).min(len)
    }

    /// Whether the buffer differs from its state at the last [`Self::mark_saved`].
    pub fn is_modified(&self) -> bool {
        self.history.current_id() != self.history.saved_id
    }

    pub fn mark_saved(&mut self) {
        self.history.saved_id = self.history.current_id();
        self.history.sealed = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// Reverts the last undo step. Returns where the cursor should go.
    pub fn undo(&mut self) -> Option<usize> {
        let t = self.history.undo.pop_back()?;
        let mut cursor = 0;
        for edit in t.edits.iter().rev() {
            self.apply(edit.at..edit.inserted_end(), &edit.removed);
            cursor = edit.removed_end();
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
            self.apply(edit.at..edit.removed_end(), &edit.inserted);
            cursor = edit.inserted_end();
        }
        self.history.undo.push_back(t);
        self.history.sealed = true;
        Some(cursor)
    }

    /// Replaces the whole text as one undo step. Only the part that actually
    /// differs (between the common prefix and suffix) is recorded. Returns
    /// whether anything changed.
    pub fn set_text(&mut self, new: &str) -> bool {
        let old = self.text.as_str();
        let prefix_bytes: usize = old
            .chars()
            .zip(new.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        let max_suffix = old.len().min(new.len()) - prefix_bytes;
        let suffix_bytes: usize = old[prefix_bytes..]
            .chars()
            .rev()
            .zip(new[prefix_bytes..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .scan(0, |total, len| {
                *total += len;
                (*total <= max_suffix).then_some(len)
            })
            .sum();
        let start = self.rope.byte_to_char(prefix_bytes);
        let end = self.rope.byte_to_char(old.len() - suffix_bytes);
        let inserted = &new[prefix_bytes..new.len() - suffix_bytes];
        self.apply_edits([(start..end, inserted)])
    }

    /// Applies replacements of char ranges in the current text as one undo
    /// step. Edits must not overlap; one that starts inside an earlier edit is
    /// skipped. Several inserts at one offset keep their order. Returns
    /// whether anything changed.
    pub fn apply_edits<S: AsRef<str>>(
        &mut self,
        edits: impl IntoIterator<Item = (Range<usize>, S)>,
    ) -> bool {
        let len = self.rope.len_chars();
        let mut sorted: Vec<(Range<usize>, S)> = edits
            .into_iter()
            .map(|(r, t)| {
                let start = r.start.min(len);
                (start..r.end.clamp(start, len), t)
            })
            .collect();
        // Stable, so several inserts at one spot keep their order.
        sorted.sort_by_key(|(r, _)| r.start);
        let mut kept = Vec::with_capacity(sorted.len());
        let mut covered = 0;
        for (range, text) in sorted {
            if range.start < covered {
                continue;
            }
            covered = range.end;
            if !range.is_empty() || !text.as_ref().is_empty() {
                kept.push((range, text));
            }
        }
        if kept.is_empty() {
            return false;
        }
        // Apply back to front so earlier offsets stay valid.
        let mut recorded = Vec::with_capacity(kept.len());
        for (range, text) in kept.into_iter().rev() {
            let edit = Edit {
                at: range.start,
                removed: self.rope.slice(range.clone()).to_string(),
                inserted: text.as_ref().to_owned(),
            };
            if edit.removed == edit.inserted {
                continue;
            }
            self.apply(range, &edit.inserted);
            recorded.push(edit);
        }
        if recorded.is_empty() {
            return false;
        }
        self.history.record_step(recorded, Instant::now());
        true
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
        let start = char_range.start.0.min(len);
        let range = start..char_range.end.0.clamp(start, len);
        self.record_and_apply(range, "");
    }

    fn char_range(&self, char_range: Range<CharIndex>) -> &str {
        let len = self.rope.len_chars();
        let start = char_range.start.0.min(len);
        let end = char_range.end.0.clamp(start, len);
        &self.text[self.rope.char_to_byte(start)..self.rope.char_to_byte(end)]
    }

    fn byte_index_from_char_index(&self, char_index: CharIndex) -> ByteIndex {
        ByteIndex(
            self.rope
                .char_to_byte(char_index.0.min(self.rope.len_chars())),
        )
    }

    fn char_index_from_byte_index(&self, byte_index: ByteIndex) -> CharIndex {
        CharIndex(self.rope.byte_to_char(byte_index.0.min(self.text.len())))
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
        assert_eq!(buf.byte_index_from_char_index(CharIndex(4)).0, 6);
        assert_eq!(buf.char_index_from_byte_index(ByteIndex(6)).0, 4);
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
        assert_eq!(Buffer::default().line_count(), 1);
        assert_eq!(Buffer::from("a\n").line_count(), 2);
    }

    #[test]
    fn char_index_inverts_line_col_and_clamps() {
        let buf = Buffer::from("ab\ncd\nef");
        for idx in 0..=8 {
            let (line, col) = buf.line_col(idx);
            assert_eq!(buf.char_index(line, col), idx);
        }
        assert_eq!(buf.char_index(1, 99), 2);
        assert_eq!(buf.char_index(9, 1), 8);
        assert_eq!(buf.char_index(0, 0), 0);
        // Only `\n` breaks lines, as in the editor widget.
        let crlf = Buffer::from("ab\r\ncd\u{2028}e");
        assert_eq!(crlf.line_count(), 2);
        assert_eq!(crlf.char_index(1, 99), 3);
        assert_eq!(crlf.line_col(8), (2, 5));
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
        assert!(!buf.can_undo());
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

    #[test]
    fn history_is_capped_without_faking_a_clean_state() {
        let mut buf = Buffer::default();
        for i in 0..MAX_UNDO_STEPS + 5 {
            buf.set_text(&i.to_string());
        }
        let mut steps = 0;
        while buf.undo().is_some() {
            steps += 1;
        }
        assert_eq!(steps, MAX_UNDO_STEPS);
        assert_eq!(buf.as_str(), "4");
        assert!(buf.is_modified(), "the original empty text is gone");
        assert_in_sync(&buf);
    }

    #[test]
    fn set_text_records_only_the_difference() {
        let mut buf = Buffer::from("fn a() {\n    é();\n}\n");
        assert!(buf.set_text("fn a() {\n    // é();\n}\n"));
        let t = buf.history.undo.back().unwrap();
        assert_eq!(
            t.edits,
            vec![Edit {
                at: 13,
                removed: String::new(),
                inserted: "// ".into()
            }]
        );
        assert!(!buf.set_text("fn a() {\n    // é();\n}\n"), "no-op");
        buf.undo();
        assert_eq!(buf.as_str(), "fn a() {\n    é();\n}\n");
        assert_in_sync(&buf);
    }

    #[test]
    fn set_text_handles_repeated_chars_at_the_seam() {
        for (old, new) in [
            ("aaa", "aa"),
            ("aa", "aaaa"),
            ("abab", "ab"),
            ("ééé", "é"),
            ("", "x"),
            ("x", ""),
        ] {
            let mut buf = Buffer::from(old);
            buf.set_text(new);
            assert_eq!(buf.as_str(), new);
            assert_in_sync(&buf);
            buf.undo();
            assert_eq!(buf.as_str(), old);
            assert_in_sync(&buf);
        }
    }

    #[test]
    fn programmatic_edits_are_their_own_step() {
        let mut buf = Buffer::default();
        type_chars(&mut buf, 0, "ab");
        buf.set_text("abc");
        type_chars(&mut buf, 3, "d");
        assert_eq!(buf.as_str(), "abcd");
        buf.undo();
        assert_eq!(buf.as_str(), "abc", "typing after a command is a new step");
        buf.undo();
        assert_eq!(buf.as_str(), "ab");
        buf.undo();
        assert_eq!(buf.as_str(), "");
    }

    #[test]
    fn apply_edits_is_one_step_and_undoes_cleanly() {
        let mut buf = Buffer::from("let ä = ä + ä;");
        let changed = buf.apply_edits([
            (12..13, "bb"),
            (4..5, "bb"),
            (8..9, "bb"),
            // Overlaps the edit at 8..9: skipped.
            (8..10, "zz"),
            // Two inserts at one spot keep their order.
            (0..0, "x"),
            (0..0, "y"),
        ]);
        assert!(changed);
        assert_eq!(buf.as_str(), "xylet bb = bb + bb;");
        assert_in_sync(&buf);
        assert_eq!(buf.history.undo.len(), 1);
        buf.undo();
        assert_eq!(buf.as_str(), "let ä = ä + ä;");
        assert_in_sync(&buf);
        buf.redo();
        assert_eq!(buf.as_str(), "xylet bb = bb + bb;");
        assert_in_sync(&buf);
    }

    #[test]
    fn out_of_range_offsets_are_clamped() {
        let mut buf = Buffer::from("ab");
        buf.insert_text("c", CharIndex(99));
        buf.delete_char_range(CharIndex(1)..CharIndex(99));
        assert_eq!(buf.as_str(), "a");
        assert!(buf.apply_edits([(5..9, "!")]));
        assert_eq!(buf.as_str(), "a!");
        assert_eq!(buf.char_range(CharIndex(1)..CharIndex(99)), "!");
        assert_in_sync(&buf);
    }
}

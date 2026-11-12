//! Rope-backed text buffer with an edit-based undo/redo history.
//!
//! The [`Rope`] is the source of truth for indexing (line/column lookups, line
//! counts, char ↔ byte conversion), which stays O(log n) regardless of file
//! size. A `String` mirror is kept in sync for the many consumers that search
//! or diff the whole text (find, git, language servers); the editor widget
//! itself only reads the lines it shows, straight from the rope.
//!
//! All mutations — typing as well as programmatic edits such as line commands,
//! replace, or language-server edits — are recorded as replacements
//! ([`Edit`]). History therefore costs memory proportional to what was
//! changed, not to the size of the file.
//!
//! Every change is also appended to a short [change log](Buffer::changes_since),
//! so views and caches built on the text (selections in other split panes,
//! folds, wrapping, syntax highlighting) can follow edits incrementally
//! instead of starting over.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ropey::Rope;

/// Consecutive edits closer together than this may be merged into one undo step.
const GROUP_TIMEOUT: Duration = Duration::from_millis(1000);

/// Upper bound on undo steps kept in memory.
const MAX_UNDO_STEPS: usize = 1000;

/// Upper bound on entries in the change log. Consumers further behind than
/// this rebuild from scratch.
const MAX_LOG: usize = 16_384;

/// A selection saved with an undo step: `(anchor, head)` char offsets per cursor.
pub type SelectionSnapshot = Vec<(usize, usize)>;

/// Versions are unique across all buffers, so `(version)` alone identifies a
/// text state, e.g. as a cache key.
fn next_version() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One applied change, as recorded in the change log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// The buffer version this change produced.
    pub version: u64,
    /// Char offset where `removed` chars were replaced by `inserted` chars.
    pub at: usize,
    pub removed: usize,
    pub inserted: usize,
    /// Line of `at`. Old lines `line..=line + removed_lines` became new lines
    /// `line..=line + inserted_lines`.
    pub line: usize,
    pub removed_lines: usize,
    pub inserted_lines: usize,
}

impl Change {
    /// Where char offset `pos` (before the change) ends up after it. Text
    /// inserted exactly at `pos` goes before it; a position inside the
    /// replaced text moves to the end of the replacement.
    pub fn map(&self, pos: usize) -> usize {
        if pos < self.at || (pos == self.at && self.removed > 0) {
            pos
        } else if pos >= self.at + self.removed {
            pos - self.removed + self.inserted
        } else {
            self.at + self.inserted
        }
    }
}

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

    fn is_insert(&self) -> bool {
        self.removed.is_empty() && !self.inserted.is_empty()
    }

    fn is_delete(&self) -> bool {
        self.inserted.is_empty() && !self.removed.is_empty()
    }

    fn has_newline(&self) -> bool {
        self.inserted.contains('\n') || self.removed.contains('\n')
    }
}

/// A group of edits undone/redone together, in the order they were applied.
#[derive(Debug)]
struct Transaction {
    /// Unique per distinct buffer state, used for dirty tracking.
    id: u64,
    edits: Vec<Edit>,
    last_edit: Instant,
    /// For typing: how many cursors each keystroke edited, so later
    /// keystrokes with the same cursors can join this step.
    typing: Option<usize>,
    /// Selections before the first edit and after the last, when the editor
    /// supplied them, restored by undo and redo.
    before: Option<SelectionSnapshot>,
    after: Option<SelectionSnapshot>,
}

impl Transaction {
    /// Whether the keystroke `next` (edits for `group` cursors) continues
    /// this transaction (typing a word, holding backspace, typing over a
    /// selection) rather than starting a new undo step.
    fn accepts(&self, next: &[Edit], group: usize, now: Instant) -> bool {
        if self.typing != Some(group) || now.duration_since(self.last_edit) > GROUP_TIMEOUT {
            return false;
        }
        let Some(last) = self.edits.last() else {
            return false;
        };
        // Start a new step at each line break so undo doesn't swallow whole paragraphs.
        let previous = &self.edits[self.edits.len().saturating_sub(group)..];
        if next.iter().chain(previous).any(Edit::has_newline) {
            return false;
        }
        if group > 1 {
            // Positions shift with every cursor; just require the same kind of keystroke.
            // Typing after typing over the selections counts as typing too.
            let typed = |e: &Edit| !e.inserted.is_empty();
            return (next.iter().all(Edit::is_insert) && previous.iter().all(typed))
                || (next.iter().all(Edit::is_delete) && previous.iter().all(Edit::is_delete));
        }
        let [next] = next else {
            return false;
        };
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

    fn push(&mut self, mut t: Transaction) {
        t.id = self.fresh_id();
        self.undo.push_back(t);
        if self.undo.len() > MAX_UNDO_STEPS
            && let Some(dropped) = self.undo.pop_front()
        {
            self.base_id = dropped.id;
        }
    }

    /// Records a keystroke's edits, merging them into the last step where
    /// they continue it.
    fn record_typing(
        &mut self,
        edits: Vec<Edit>,
        group: usize,
        now: Instant,
        before: Option<SelectionSnapshot>,
        after: Option<SelectionSnapshot>,
    ) {
        self.redo.clear();
        let id = self.fresh_id();
        let saved_id = self.saved_id;
        match self.undo.back_mut() {
            Some(t) if !self.sealed && t.id != saved_id && t.accepts(&edits, group, now) => {
                t.edits.extend(edits);
                t.last_edit = now;
                if after.is_some() {
                    t.after = after;
                }
                // The merged transaction now describes a different state.
                t.id = id;
            }
            _ => self.push(Transaction {
                id: 0,
                edits,
                last_edit: now,
                typing: Some(group),
                before,
                after,
            }),
        }
        self.sealed = false;
    }

    /// Records edits as one step of their own; the next edit starts another.
    fn record_step(
        &mut self,
        edits: Vec<Edit>,
        now: Instant,
        before: Option<SelectionSnapshot>,
        after: Option<SelectionSnapshot>,
    ) {
        self.redo.clear();
        self.push(Transaction {
            id: 0,
            edits,
            last_edit: now,
            typing: None,
            before,
            after,
        });
        self.sealed = true;
    }
}

/// How [`Buffer::edit`] records its change in the undo history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// A keystroke: may merge with the previous keystrokes into one undo step.
    Typing,
    /// A command: always an undo step of its own.
    Command,
}

pub struct Buffer {
    rope: Rope,
    text: String,
    history: History,
    version: u64,
    log: VecDeque<Change>,
    /// Version before the oldest change in `log`.
    log_base: u64,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::from("")
    }
}

impl From<&str> for Buffer {
    fn from(text: &str) -> Self {
        let version = next_version();
        Self {
            rope: Rope::from_str(text),
            text: text.to_owned(),
            history: History::default(),
            version,
            log: VecDeque::new(),
            log_base: version,
        }
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("len_chars", &self.rope.len_chars())
            .field("version", &self.version)
            .field("undo_steps", &self.history.undo.len())
            .field("redo_steps", &self.history.redo.len())
            .finish()
    }
}

impl Buffer {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Identifies the current text: changes with every edit, and is never
    /// shared with another buffer.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The changes applied after `version`, oldest first, or `None` if that
    /// is too far back (or not a version of this buffer) to tell.
    pub fn changes_since(&self, version: u64) -> Option<impl Iterator<Item = &Change>> {
        if version == self.version {
            return Some(self.log.range(self.log.len()..));
        }
        if version < self.log_base || version > self.version {
            return None;
        }
        let first = self.log.partition_point(|c| c.version <= version);
        // `version` must be one this buffer was at.
        let known = version == self.log_base
            || self
                .log
                .get(first.wrapping_sub(1))
                .is_some_and(|c| c.version == version);
        known.then(|| self.log.range(first..))
    }

    /// Number of lines; an empty buffer, or one ending in `\n`, counts the final empty line.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Char offset where 0-based `line` starts (clamped to the last line).
    pub fn line_start(&self, line: usize) -> usize {
        self.rope
            .line_to_char(line.min(self.rope.len_lines().saturating_sub(1)))
    }

    /// Length of 0-based `line` in chars, without its line break.
    pub fn line_len(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return 0;
        }
        let start = self.rope.line_to_char(line);
        // Lines break only on `\n` (see Cargo.toml), so every line but the
        // last ends in exactly one.
        if line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(line + 1) - start - 1
        } else {
            self.rope.len_chars() - start
        }
    }

    /// 0-based line containing char offset `char_idx` (clamped).
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.rope.len_chars()))
    }

    /// The text of 0-based `line`, without its line break.
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.rope.len_lines() {
            return String::new();
        }
        let start = self.rope.line_to_char(line);
        self.slice(start..start + self.line_len(line))
    }

    /// The text in a char range (clamped).
    pub fn slice(&self, range: Range<usize>) -> String {
        let len = self.rope.len_chars();
        let start = range.start.min(len);
        let end = range.end.clamp(start, len);
        let (a, b) = (self.rope.char_to_byte(start), self.rope.char_to_byte(end));
        self.text[a..b].to_owned()
    }

    /// The char at `idx`, if any.
    pub fn char_at(&self, idx: usize) -> Option<char> {
        (idx < self.rope.len_chars()).then(|| self.rope.char(idx))
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
        self.rope.line_to_char(line) + col.saturating_sub(1).min(self.line_len(line))
    }

    /// Whether the buffer differs from its state at the last [`Self::mark_saved`].
    pub fn is_modified(&self) -> bool {
        self.history.current_id() != self.history.saved_id
    }

    pub fn mark_saved(&mut self) {
        self.history.saved_id = self.history.current_id();
        self.history.sealed = true;
    }

    /// Makes the next edit start a new undo step.
    pub fn seal(&mut self) {
        self.history.sealed = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// Reverts the last undo step. Returns the selections to restore: those
    /// saved with the step, or a caret where the change was.
    pub fn undo(&mut self) -> Option<SelectionSnapshot> {
        let t = self.history.undo.pop_back()?;
        let mut cursor = 0;
        for edit in t.edits.iter().rev() {
            self.apply(edit.at..edit.inserted_end(), &edit.removed);
            cursor = edit.removed_end();
        }
        let restore = t.before.clone().unwrap_or_else(|| vec![(cursor, cursor)]);
        self.history.redo.push(t);
        self.history.sealed = true;
        Some(restore)
    }

    /// Re-applies the last undone step. Returns the selections to restore.
    pub fn redo(&mut self) -> Option<SelectionSnapshot> {
        let t = self.history.redo.pop()?;
        let mut cursor = 0;
        for edit in &t.edits {
            self.apply(edit.at..edit.removed_end(), &edit.inserted);
            cursor = edit.inserted_end();
        }
        let restore = t.after.clone().unwrap_or_else(|| vec![(cursor, cursor)]);
        self.history.undo.push_back(t);
        self.history.sealed = true;
        Some(restore)
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
        self.edit(edits, EditKind::Command, None)
    }

    /// Applies replacements like [`Self::apply_edits`], recorded as `kind`.
    /// `selections` (before, after) are restored by undo and redo.
    pub fn edit<S: AsRef<str>>(
        &mut self,
        edits: impl IntoIterator<Item = (Range<usize>, S)>,
        kind: EditKind,
        selections: Option<(SelectionSnapshot, SelectionSnapshot)>,
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
        let group = kept.len();
        // Apply back to front so earlier offsets stay valid.
        let mut recorded = Vec::with_capacity(kept.len());
        for (range, text) in kept.into_iter().rev() {
            let edit = Edit {
                at: range.start,
                removed: self.slice(range.clone()),
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
        let (before, after) = selections.unzip();
        match kind {
            EditKind::Typing => {
                self.history
                    .record_typing(recorded, group, Instant::now(), before, after)
            }
            EditKind::Command => self
                .history
                .record_step(recorded, Instant::now(), before, after),
        }
        true
    }

    /// Applies a change to the rope and the string mirror without recording
    /// it in the history; logs it for views and caches.
    fn apply(&mut self, range: Range<usize>, text: &str) {
        let start_byte = self.rope.char_to_byte(range.start);
        let end_byte = self.rope.char_to_byte(range.end);
        let line = self.rope.char_to_line(range.start);
        let removed_lines = self.rope.char_to_line(range.end) - line;
        self.rope.remove(range.clone());
        self.rope.insert(range.start, text);
        self.text.replace_range(start_byte..end_byte, text);

        self.version = next_version();
        self.log.push_back(Change {
            version: self.version,
            at: range.start,
            removed: range.len(),
            inserted: text.chars().count(),
            line,
            removed_lines,
            inserted_lines: text.matches('\n').count(),
        });
        if self.log.len() > MAX_LOG
            && let Some(dropped) = self.log.pop_front()
        {
            self.log_base = dropped.version;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(buf: &mut Buffer, at: usize, text: &str) {
        buf.edit([(at..at, text)], EditKind::Typing, None);
    }

    fn delete(buf: &mut Buffer, range: Range<usize>) {
        buf.edit([(range, "")], EditKind::Typing, None);
    }

    fn type_chars(buf: &mut Buffer, at: usize, text: &str) {
        for (i, c) in text.chars().enumerate() {
            insert(buf, at + i, &c.to_string());
        }
    }

    fn caret(at: usize) -> Option<SelectionSnapshot> {
        Some(vec![(at, at)])
    }

    fn assert_in_sync(buf: &Buffer) {
        assert_eq!(buf.rope.to_string(), buf.text);
    }

    #[test]
    fn edits_keep_rope_and_string_in_sync() {
        let mut buf = Buffer::from("héllo wörld");
        insert(&mut buf, 5, "✨");
        delete(&mut buf, 0..2);
        assert_eq!(buf.as_str(), "llo✨ wörld");
        assert_in_sync(&buf);
        assert_eq!(buf.slice(3..4), "✨");
        assert_eq!(buf.char_at(3), Some('✨'));
        assert_eq!(buf.line_text(0), "llo✨ wörld");
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
        assert_eq!(buf.undo(), caret(0));
        assert_eq!(buf.as_str(), "");
        assert_eq!(buf.redo(), caret(5));
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
            delete(&mut buf, end - 1..end);
        }
        assert_eq!(buf.as_str(), "");
        assert_eq!(buf.undo(), caret(5));
        assert_eq!(buf.as_str(), "hello");
    }

    #[test]
    fn typing_over_selection_is_one_undo_step() {
        let mut buf = Buffer::from("hello world");
        delete(&mut buf, 6..11);
        type_chars(&mut buf, 6, "there");
        assert_eq!(buf.as_str(), "hello there");
        buf.undo();
        assert_eq!(buf.as_str(), "hello world");
        assert!(!buf.can_undo());
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut buf = Buffer::default();
        insert(&mut buf, 0, "a");
        buf.undo();
        assert!(buf.can_redo());
        insert(&mut buf, 0, "b");
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
        insert(&mut buf, 99, "c");
        delete(&mut buf, 1..99);
        assert_eq!(buf.as_str(), "a");
        assert!(buf.apply_edits([(5..9, "!")]));
        assert_eq!(buf.as_str(), "a!");
        assert_eq!(buf.slice(1..99), "!");
        assert_in_sync(&buf);
    }

    #[test]
    fn typing_at_several_cursors_groups_and_restores_selections() {
        let mut buf = Buffer::from("a\nb\nc");
        let before = vec![(1, 1), (3, 3), (5, 5)];
        let mut sels = before.clone();
        for ch in ["x", "y"] {
            let edits: Vec<_> = sels.iter().map(|&(a, _)| (a..a, ch)).collect();
            // Each cursor moves past its own insert and those before it.
            let after: SelectionSnapshot = sels
                .iter()
                .enumerate()
                .map(|(i, &(a, _))| (a + i + 1, a + i + 1))
                .collect();
            buf.edit(edits, EditKind::Typing, Some((sels.clone(), after.clone())));
            sels = after;
        }
        assert_eq!(buf.as_str(), "axy\nbxy\ncxy");
        assert_eq!(buf.history.undo.len(), 1, "one undo step for the word");
        assert_eq!(buf.undo(), Some(before));
        assert_eq!(buf.as_str(), "a\nb\nc");
        assert_eq!(buf.redo(), Some(sels));
        assert_in_sync(&buf);
    }

    #[test]
    fn typing_over_several_selections_is_one_step() {
        let mut buf = Buffer::from("a = 1\nb = 2");
        buf.edit([(2..3, ":"), (8..9, ":")], EditKind::Typing, None);
        buf.edit([(3..3, "="), (9..9, "=")], EditKind::Typing, None);
        assert_eq!(buf.as_str(), "a := 1\nb := 2");
        buf.undo();
        assert_eq!(buf.as_str(), "a = 1\nb = 2");
    }

    #[test]
    fn multi_cursor_insert_then_delete_are_separate_steps() {
        let mut buf = Buffer::from("ab");
        buf.edit([(0..0, "x"), (1..1, "x")], EditKind::Typing, None);
        buf.edit([(0..1, ""), (2..3, "")], EditKind::Typing, None);
        assert_eq!(buf.as_str(), "ab");
        buf.undo();
        assert_eq!(buf.as_str(), "xaxb");
        buf.undo();
        assert_eq!(buf.as_str(), "ab");
    }

    #[test]
    fn change_log_tracks_lines_and_versions() {
        let mut buf = Buffer::from("one\ntwo\nthree");
        let v0 = buf.version();
        assert_eq!(buf.changes_since(v0).unwrap().count(), 0);
        buf.apply_edits([(4..7, "2\n2b")]);
        let v1 = buf.version();
        assert_ne!(v0, v1);
        insert(&mut buf, 0, "x");
        let all: Vec<Change> = buf.changes_since(v0).unwrap().copied().collect();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].at, all[0].removed, all[0].inserted), (4, 3, 4));
        assert_eq!(
            (all[0].line, all[0].removed_lines, all[0].inserted_lines),
            (1, 0, 1)
        );
        let later: Vec<Change> = buf.changes_since(v1).unwrap().copied().collect();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].at, 0);
        // Another buffer's versions are unknown here.
        assert!(buf.changes_since(Buffer::from("x").version()).is_none());
        // Undo is logged too.
        buf.undo();
        assert_eq!(buf.changes_since(v1).unwrap().count(), 2);
    }

    #[test]
    fn change_maps_positions() {
        let c = Change {
            version: 0,
            at: 5,
            removed: 3,
            inserted: 1,
            line: 0,
            removed_lines: 0,
            inserted_lines: 0,
        };
        assert_eq!(c.map(2), 2);
        assert_eq!(c.map(5), 5, "start of replaced text stays");
        assert_eq!(c.map(6), 6, "inside moves to end of replacement");
        assert_eq!(c.map(8), 6);
        assert_eq!(c.map(10), 8);
        let insert = Change {
            removed: 0,
            inserted: 2,
            ..c
        };
        assert_eq!(insert.map(5), 7, "insert at a position goes before it");
    }

    #[test]
    fn line_helpers() {
        let buf = Buffer::from("ab\n\ncdé");
        assert_eq!(buf.line_len(0), 2);
        assert_eq!(buf.line_len(1), 0);
        assert_eq!(buf.line_len(2), 3);
        assert_eq!(buf.line_len(9), 0);
        assert_eq!(buf.line_start(2), 4);
        assert_eq!(buf.line_text(2), "cdé");
        assert_eq!(buf.char_to_line(4), 2);
    }
}

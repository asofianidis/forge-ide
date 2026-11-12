use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use anyhow::Context as _;

use super::{Buffer, HighlightCache, ops};
use crate::editorconfig::EditorConfig;

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// How the file on disk relates to the buffer, as of the last [`Document::poll_disk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskState {
    /// The file matches what was last loaded or saved.
    InSync,
    /// The file was changed by another program while the buffer had unsaved
    /// edits. Holds the new file contents so the user can pick a version.
    Changed(String),
    /// The file was deleted (or moved) by another program.
    Deleted,
}

/// What [`Document::poll_disk`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskEvent {
    /// The buffer was clean, so it now shows the new file contents.
    Reloaded,
    /// The file changed but the buffer has unsaved edits; see [`DiskState::Changed`].
    Conflict,
    Deleted,
}

/// A single open file (or unsaved scratch buffer).
pub struct Document {
    /// Unique for the session; keys per-document widget state such as the cursor.
    pub id: u64,
    pub path: Option<PathBuf>,
    /// The text and its undo history. The editor widget edits it directly;
    /// everything else should go through [`Self::set_text`] or
    /// [`Self::apply_edits`] so each change is one undo step.
    pub buffer: Buffer,
    /// Syntax highlighting, filled in lazily as lines are shown.
    pub highlight: HighlightCache,
    /// Hash of the text as of the last load/save, to tell whether the file on
    /// disk still matches it without keeping a second copy of the text.
    saved_hash: u64,
    /// Modification time of the file as of the last load, save or poll.
    disk_mtime: Option<SystemTime>,
    pub disk: DiskState,
    /// `.editorconfig` settings for the file's path.
    pub editorconfig: EditorConfig,
}

impl Document {
    pub fn untitled() -> Self {
        Self {
            id: next_id(),
            path: None,
            buffer: Buffer::default(),
            highlight: HighlightCache::default(),
            saved_hash: hash(""),
            disk_mtime: None,
            disk: DiskState::InSync,
            editorconfig: EditorConfig::default(),
        }
    }

    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            id: next_id(),
            path: Some(path.to_path_buf()),
            saved_hash: hash(&text),
            buffer: Buffer::from(text.as_str()),
            highlight: HighlightCache::default(),
            disk_mtime: mtime(path),
            disk: DiskState::InSync,
            editorconfig: EditorConfig::for_file(path),
        })
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        let path = self.path.as_ref().context("document has no path")?;
        std::fs::write(path, self.text())
            .with_context(|| format!("failed to write {}", path.display()))?;
        self.saved_hash = hash(self.text());
        self.buffer.mark_saved();
        self.disk_mtime = mtime(path);
        self.disk = DiskState::InSync;
        Ok(())
    }

    /// Points the document at a new path (for Save As), picking up that
    /// path's `.editorconfig` settings. Doesn't write anything.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.editorconfig = path
            .as_deref()
            .map(EditorConfig::for_file)
            .unwrap_or_default();
        self.path = path;
    }

    /// Re-reads the `.editorconfig` settings, after one of the files changed.
    pub fn refresh_editorconfig(&mut self) {
        if let Some(path) = &self.path {
            self.editorconfig = EditorConfig::for_file(path);
        }
    }

    /// The indentation to insert: from `.editorconfig`, else what the file
    /// already uses.
    pub fn indent_unit(&self) -> String {
        self.editorconfig
            .indent_unit(&ops::indent_unit(self.text()))
    }

    /// Tab width in columns, for language servers' formatting options.
    pub fn tab_width(&self) -> usize {
        self.editorconfig.tab_width().unwrap_or(4)
    }

    /// Unsaved edits, or a buffer whose file was deleted from under it.
    pub fn is_dirty(&self) -> bool {
        self.has_unsaved_edits() || self.disk == DiskState::Deleted
    }

    /// Whether the buffer differs from what was last loaded or saved. Undoing
    /// back to that state counts as unchanged.
    pub fn has_unsaved_edits(&self) -> bool {
        self.buffer.is_modified()
    }

    pub fn text(&self) -> &str {
        self.buffer.as_str()
    }

    /// Changes with every edit; identifies the text for caches.
    pub fn version(&self) -> u64 {
        self.buffer.version()
    }

    /// Replaces the text as one undo step (recording only what differs).
    /// Returns whether anything changed.
    pub fn set_text(&mut self, text: &str) -> bool {
        self.buffer.set_text(text)
    }

    /// Applies non-overlapping replacements of char ranges as one undo step.
    /// Returns whether anything changed.
    pub fn apply_edits<S: AsRef<str>>(
        &mut self,
        edits: impl IntoIterator<Item = (Range<usize>, S)>,
    ) -> bool {
        self.buffer.apply_edits(edits)
    }

    /// Shows `disk_text` as the saved state. The reload is an undo step of its
    /// own, so the previous buffer can be brought back (it then counts as
    /// unsaved).
    fn load_from_disk(&mut self, disk_text: &str) {
        self.buffer.set_text(disk_text);
        self.buffer.mark_saved();
        self.saved_hash = hash(disk_text);
    }

    /// Checks whether another program changed or deleted the file.
    ///
    /// A clean buffer is reloaded in place. A dirty one is left alone and
    /// marked [`DiskState::Changed`] until the user resolves it.
    pub fn poll_disk(&mut self) -> Option<DiskEvent> {
        let path = self.path.as_deref()?;
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if self.disk == DiskState::Deleted {
                    return None;
                }
                self.disk_mtime = None;
                self.disk = DiskState::Deleted;
                return Some(DiskEvent::Deleted);
            }
            Err(_) => return None,
        };
        let modified = meta.modified().ok();
        if modified == self.disk_mtime && self.disk != DiskState::Deleted {
            return None;
        }
        // A file mid-write may not be valid UTF-8 yet; try again next poll.
        let disk_text = std::fs::read_to_string(path).ok()?;
        self.disk_mtime = modified;
        if hash(&disk_text) == self.saved_hash {
            // Touched, or restored to what we had: nothing to resolve.
            self.disk = DiskState::InSync;
            return None;
        }
        if !self.has_unsaved_edits() {
            self.load_from_disk(&disk_text);
            self.disk = DiskState::InSync;
            Some(DiskEvent::Reloaded)
        } else {
            self.disk = DiskState::Changed(disk_text);
            Some(DiskEvent::Conflict)
        }
    }

    /// Resolves a [`DiskState::Changed`] conflict by taking the file's contents.
    pub fn reload_from_disk(&mut self) {
        if let DiskState::Changed(disk_text) = std::mem::replace(&mut self.disk, DiskState::InSync)
        {
            self.load_from_disk(&disk_text);
        }
    }

    /// Resolves a [`DiskState::Changed`] conflict by keeping the buffer; the
    /// next save overwrites the file.
    pub fn keep_local(&mut self) {
        if matches!(self.disk, DiskState::Changed(_)) {
            self.disk = DiskState::InSync;
        }
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_owned())
    }

    /// File extension used to pick a syntax definition.
    pub fn extension(&self) -> &str {
        self.path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
    }

    /// Name used to pick a comment style: the extension, or the file name for
    /// extension-less files such as `Makefile`.
    pub fn language_key(&self) -> String {
        match self.path.as_ref().and_then(|p| p.extension()) {
            Some(ext) => ext.to_string_lossy().into_owned(),
            None => self.title().to_lowercase(),
        }
    }

    /// Number of lines in the buffer (an empty buffer has one line).
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// 1-based (line, column) of a character offset.
    pub fn line_col(&self, char_idx: usize) -> (usize, usize) {
        self.buffer.line_col(char_idx)
    }

    /// Character offset of a 1-based (line, column), clamped to the text.
    pub fn char_index(&self, line: usize, col: usize) -> usize {
        self.buffer.char_index(line, col)
    }
}

fn hash(text: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_newlines() {
        let mut doc = Document::untitled();
        doc.set_text("ab\ncd\nef");
        assert_eq!(doc.line_col(0), (1, 1));
        assert_eq!(doc.line_col(2), (1, 3));
        assert_eq!(doc.line_col(3), (2, 1));
        assert_eq!(doc.line_col(7), (3, 2));
    }

    #[test]
    fn char_index_inverts_line_col_and_clamps() {
        let mut doc = Document::untitled();
        doc.set_text("ab\ncd\nef");
        for idx in 0..=8 {
            let (line, col) = doc.line_col(idx);
            assert_eq!(doc.char_index(line, col), idx);
        }
        // Past the end of a line, or past the last line (the file got shorter).
        assert_eq!(doc.char_index(1, 99), 2);
        assert_eq!(doc.char_index(9, 1), 8);
        assert_eq!(doc.char_index(0, 0), 0);
    }

    #[test]
    fn line_count_includes_trailing_empty_line() {
        let mut doc = Document::untitled();
        assert_eq!(doc.line_count(), 1);
        doc.set_text("a\nb");
        assert_eq!(doc.line_count(), 2);
        doc.set_text("a\nb\n");
        assert_eq!(doc.line_count(), 3);
    }

    #[test]
    fn dirty_tracking() {
        let mut doc = Document::untitled();
        assert!(!doc.is_dirty());
        doc.buffer
            .edit([(0..0, "x")], crate::editor::EditKind::Typing, None);
        assert!(doc.is_dirty());
        doc.buffer.undo();
        assert!(!doc.is_dirty(), "undoing back to the saved text is clean");
        doc.set_text("y");
        assert!(doc.is_dirty());
    }

    #[test]
    fn save_marks_clean_and_undo_past_it_is_dirty() {
        let file = TempFile::new("save", "a");
        let mut doc = Document::open(&file.0).unwrap();
        doc.set_text("ab");
        doc.save().unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(std::fs::read_to_string(&file.0).unwrap(), "ab");
        doc.buffer.undo();
        assert_eq!(doc.text(), "a");
        assert!(doc.is_dirty());
        doc.buffer.redo();
        assert!(!doc.is_dirty());
    }

    /// A scratch file whose mtime can be bumped deterministically.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, text: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("forge-doc-test-{}-{name}", std::process::id()));
            std::fs::write(&path, text).unwrap();
            Self(path)
        }

        /// Rewrites the file as another program would, with a later mtime.
        fn write_externally(&self, text: &str) {
            std::fs::write(&self.0, text).unwrap();
            let later = SystemTime::now() + std::time::Duration::from_secs(5);
            std::fs::File::options()
                .write(true)
                .open(&self.0)
                .unwrap()
                .set_modified(later)
                .unwrap();
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn unchanged_file_is_quiet() {
        let file = TempFile::new("quiet", "a");
        let mut doc = Document::open(&file.0).unwrap();
        assert_eq!(doc.poll_disk(), None);
        doc.set_text("ab");
        doc.save().unwrap();
        assert_eq!(
            doc.poll_disk(),
            None,
            "our own save is not an external change"
        );
    }

    #[test]
    fn clean_buffer_reloads() {
        let file = TempFile::new("reload", "old");
        let mut doc = Document::open(&file.0).unwrap();
        file.write_externally("new");
        assert_eq!(doc.poll_disk(), Some(DiskEvent::Reloaded));
        assert_eq!(doc.text(), "new");
        assert!(!doc.is_dirty());
        assert_eq!(doc.poll_disk(), None);
        // The reload is undoable; the old text then counts as unsaved.
        doc.buffer.undo();
        assert_eq!(doc.text(), "old");
        assert!(doc.is_dirty());
        doc.buffer.redo();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn dirty_buffer_conflicts_and_resolves() {
        let file = TempFile::new("conflict", "old");
        let mut doc = Document::open(&file.0).unwrap();
        doc.set_text("mine");
        file.write_externally("theirs");
        assert_eq!(doc.poll_disk(), Some(DiskEvent::Conflict));
        assert_eq!(doc.text(), "mine", "unsaved edits are never overwritten");
        assert_eq!(doc.disk, DiskState::Changed("theirs".into()));

        doc.reload_from_disk();
        assert_eq!(doc.text(), "theirs");
        assert!(!doc.is_dirty());
        assert_eq!(doc.disk, DiskState::InSync);
        // Taking the file's version doesn't lose the local edits for good.
        doc.buffer.undo();
        assert_eq!(doc.text(), "mine");
        assert!(doc.is_dirty());
    }

    #[test]
    fn keep_local_leaves_buffer_dirty() {
        let file = TempFile::new("keep", "old");
        let mut doc = Document::open(&file.0).unwrap();
        doc.set_text("mine");
        file.write_externally("theirs");
        doc.poll_disk();
        doc.keep_local();
        assert_eq!(doc.disk, DiskState::InSync);
        assert!(doc.is_dirty());
        assert_eq!(
            doc.poll_disk(),
            None,
            "a resolved change is not reported again"
        );
    }

    #[test]
    fn deleted_file_marks_buffer_dirty_until_saved() {
        let file = TempFile::new("deleted", "text");
        let mut doc = Document::open(&file.0).unwrap();
        std::fs::remove_file(&file.0).unwrap();
        assert_eq!(doc.poll_disk(), Some(DiskEvent::Deleted));
        assert_eq!(doc.poll_disk(), None);
        assert!(doc.is_dirty());
        doc.save().unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(doc.disk, DiskState::InSync);
    }

    #[test]
    fn restored_file_clears_deleted_state() {
        let file = TempFile::new("restored", "text");
        let mut doc = Document::open(&file.0).unwrap();
        std::fs::remove_file(&file.0).unwrap();
        doc.poll_disk();
        file.write_externally("text");
        assert_eq!(doc.poll_disk(), None);
        assert!(!doc.is_dirty());
    }

    /// The editor's commands compute a new text (or char edits) and hand it to
    /// the document, as `app` does. Each must be one undo step that restores
    /// the text exactly, with the rope kept in sync for line/column lookups.
    mod programmatic_edits {
        use super::*;
        use crate::editor::{conflicts, find, ops};
        use crate::lsp::text;

        /// Applies `new` the way the app does, then checks it undoes and
        /// redoes as one step.
        fn check_one_step(before: &str, new: &str) {
            let mut doc = Document::untitled();
            doc.buffer = Buffer::from(before);
            assert!(doc.set_text(new));
            assert_eq!(doc.text(), new);
            assert!(doc.is_dirty());
            let caret = doc.buffer.undo().expect("undoable");
            assert!(
                caret
                    .iter()
                    .all(|&(a, h)| a.max(h) <= before.chars().count())
            );
            assert_eq!(doc.text(), before);
            assert!(!doc.is_dirty());
            assert!(!doc.buffer.can_undo(), "{new:?} took more than one step");
            doc.buffer.redo();
            assert_eq!(doc.text(), new);
            let last = new.chars().count();
            assert_eq!(doc.line_col(last).0, new.split('\n').count());
        }

        #[test]
        fn line_commands() {
            let text = "fn main() {\n    let é = 1;\n    let ✨ = 2;\n}\n";
            let sel = ops::Selection::new(16, 30);
            let style = ops::comment_style("rs").unwrap();
            check_one_step(text, &ops::toggle_comment(text, sel, style).text);
            check_one_step(text, &ops::indent(text, sel, "    ").text);
            check_one_step(text, &ops::outdent(text, sel, "    ").text);
            check_one_step(text, &ops::duplicate_lines(text, sel).text);
            check_one_step(text, &ops::move_lines(text, sel, true).unwrap().text);
            check_one_step(text, &ops::newline(text, sel, "    ", "rs").text);
            let typed = ops::type_char(text, ops::Selection::caret(11), '(', &[]).unwrap();
            check_one_step(text, &typed.text);
        }

        #[test]
        fn replace_and_replace_all() {
            let text = "aé aé\naé";
            let re = find::build_regex("aé", find::FindOptions::default()).unwrap();
            let matches = find::find_all(text, &re);
            let (one, _) = find::replace_one(text, &re, &matches[1], "✨", false);
            check_one_step(text, &one);
            let (all, count) = find::replace_all(text, &re, "b", false);
            assert_eq!(count, 3);
            check_one_step(text, &all);
        }

        #[test]
        fn conflict_resolution() {
            let text = "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> b\nz\n";
            let found = conflicts::find(text);
            for how in [
                conflicts::Resolution::Current,
                conflicts::Resolution::Incoming,
                conflicts::Resolution::Both,
            ] {
                check_one_step(text, &conflicts::resolve(text, &found[0], how));
                check_one_step(text, &conflicts::resolve_all(text, how));
            }
        }

        #[test]
        fn hunk_revert() {
            let base = "one\ntwo\nthree\n";
            let text = "one\nTWO ✨\nthree\nfour\n";
            let changes = crate::git::line_changes::compute(base, text);
            for hunk in &changes.hunks {
                check_one_step(text, &crate::git::line_changes::revert(text, hunk));
            }
        }

        #[test]
        fn language_server_edits() {
            use lsp_types::{Position, Range, TextEdit};
            let text = "let 🦀 = 1;\nlet b = 🦀;\n";
            let edit = |l1, c1, l2, c2, t: &str| TextEdit {
                range: Range::new(Position::new(l1, c1), Position::new(l2, c2)),
                new_text: t.to_owned(),
            };
            // A rename (UTF-16 offsets: the emoji is two units), plus an
            // insert at the top as a completion's extra edit would make.
            let edits = [
                edit(0, 4, 0, 6, "x"),
                edit(1, 8, 1, 10, "x"),
                edit(0, 0, 0, 0, "use y;\n"),
            ];
            let char_edits = text::char_edits(text, &edits, text::Encoding::Utf16);
            let expected = text::apply_edits(text, &char_edits);
            assert_eq!(expected, "use y;\nlet x = 1;\nlet b = x;\n");

            let mut doc = Document::untitled();
            doc.buffer = Buffer::from(text);
            assert!(doc.apply_edits(char_edits.iter().map(|e| (e.range.clone(), &e.text))));
            assert_eq!(doc.text(), expected);
            assert_eq!(doc.line_col(doc.text().chars().count()), (4, 1));
            doc.buffer.undo();
            assert_eq!(doc.text(), text);
            assert!(!doc.is_dirty());
            assert!(!doc.buffer.can_undo());
        }
    }
}

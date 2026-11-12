use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use anyhow::Context as _;

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
    pub text: String,
    /// Text as of the last load/save, used to compute the dirty flag.
    saved_text: String,
    /// Modification time of the file as of the last load, save or poll.
    disk_mtime: Option<SystemTime>,
    pub disk: DiskState,
}

impl Document {
    pub fn untitled() -> Self {
        Self {
            id: next_id(),
            path: None,
            text: String::new(),
            saved_text: String::new(),
            disk_mtime: None,
            disk: DiskState::InSync,
        }
    }

    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            id: next_id(),
            path: Some(path.to_path_buf()),
            saved_text: text.clone(),
            text,
            disk_mtime: mtime(path),
            disk: DiskState::InSync,
        })
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        let path = self.path.as_ref().context("document has no path")?;
        std::fs::write(path, &self.text)
            .with_context(|| format!("failed to write {}", path.display()))?;
        self.saved_text.clone_from(&self.text);
        self.disk_mtime = mtime(path);
        self.disk = DiskState::InSync;
        Ok(())
    }

    pub fn save_as(&mut self, path: PathBuf) -> anyhow::Result<()> {
        self.path = Some(path);
        self.save()
    }

    /// Unsaved edits, or a buffer whose file was deleted from under it.
    pub fn is_dirty(&self) -> bool {
        self.text != self.saved_text || self.disk == DiskState::Deleted
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
        if disk_text == self.saved_text {
            // Touched, or restored to what we had: nothing to resolve.
            self.disk = DiskState::InSync;
            return None;
        }
        if self.text == self.saved_text {
            self.text.clone_from(&disk_text);
            self.saved_text = disk_text;
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
            self.text.clone_from(&disk_text);
            self.saved_text = disk_text;
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
        self.text.bytes().filter(|&b| b == b'\n').count() + 1
    }

    /// 1-based (line, column) of a character offset.
    pub fn line_col(&self, char_idx: usize) -> (usize, usize) {
        let mut line = 1;
        let mut col = 1;
        for c in self.text.chars().take(char_idx) {
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
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
        doc.text = "ab\ncd\nef".into();
        assert_eq!(doc.line_col(0), (1, 1));
        assert_eq!(doc.line_col(2), (1, 3));
        assert_eq!(doc.line_col(3), (2, 1));
        assert_eq!(doc.line_col(7), (3, 2));
    }

    #[test]
    fn line_count_includes_trailing_empty_line() {
        let mut doc = Document::untitled();
        assert_eq!(doc.line_count(), 1);
        doc.text = "a\nb".into();
        assert_eq!(doc.line_count(), 2);
        doc.text.push('\n');
        assert_eq!(doc.line_count(), 3);
    }

    #[test]
    fn dirty_tracking() {
        let mut doc = Document::untitled();
        assert!(!doc.is_dirty());
        doc.text.push('x');
        assert!(doc.is_dirty());
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
        doc.text.push('b');
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
        assert_eq!(doc.text, "new");
        assert!(!doc.is_dirty());
        assert_eq!(doc.poll_disk(), None);
    }

    #[test]
    fn dirty_buffer_conflicts_and_resolves() {
        let file = TempFile::new("conflict", "old");
        let mut doc = Document::open(&file.0).unwrap();
        doc.text = "mine".into();
        file.write_externally("theirs");
        assert_eq!(doc.poll_disk(), Some(DiskEvent::Conflict));
        assert_eq!(doc.text, "mine", "unsaved edits are never overwritten");
        assert_eq!(doc.disk, DiskState::Changed("theirs".into()));

        doc.reload_from_disk();
        assert_eq!(doc.text, "theirs");
        assert!(!doc.is_dirty());
        assert_eq!(doc.disk, DiskState::InSync);
    }

    #[test]
    fn keep_local_leaves_buffer_dirty() {
        let file = TempFile::new("keep", "old");
        let mut doc = Document::open(&file.0).unwrap();
        doc.text = "mine".into();
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
}

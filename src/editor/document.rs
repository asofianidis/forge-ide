use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use egui::TextBuffer as _;

use super::Buffer;

/// A single open file (or unsaved scratch buffer).
pub struct Document {
    /// Stable identity for this document's editor widget state.
    pub id: egui::Id,
    pub path: Option<PathBuf>,
    pub buffer: Buffer,
}

fn next_id() -> egui::Id {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    egui::Id::unique(("document", NEXT.fetch_add(1, Ordering::Relaxed)))
}

impl Document {
    pub fn untitled() -> Self {
        Self {
            id: next_id(),
            path: None,
            buffer: Buffer::default(),
        }
    }

    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            id: next_id(),
            path: Some(path.to_path_buf()),
            buffer: Buffer::from(text.as_str()),
        })
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        let path = self.path.as_ref().context("document has no path")?;
        std::fs::write(path, self.buffer.as_str())
            .with_context(|| format!("failed to write {}", path.display()))?;
        self.buffer.mark_saved();
        Ok(())
    }

    pub fn save_as(&mut self, path: PathBuf) -> anyhow::Result<()> {
        self.path = Some(path);
        self.save()
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_modified()
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

    /// Number of lines in the buffer (an empty buffer has one line).
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// 1-based (line, column) of a character offset.
    pub fn line_col(&self, char_idx: usize) -> (usize, usize) {
        self.buffer.line_col(char_idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::text::CharIndex;

    #[test]
    fn dirty_tracking() {
        let mut doc = Document::untitled();
        assert!(!doc.is_dirty());
        doc.buffer.insert_text("x", CharIndex(0));
        assert!(doc.is_dirty());
        doc.buffer.undo();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn save_marks_clean() {
        let dir = std::env::temp_dir().join(format!("forge-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        let mut doc = Document::untitled();
        doc.buffer.insert_text("hello", CharIndex(0));
        doc.save_as(path.clone()).unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        assert_eq!(Document::open(&path).unwrap().buffer.as_str(), "hello");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// A single open file (or unsaved scratch buffer).
pub struct Document {
    pub path: Option<PathBuf>,
    pub text: String,
    /// Text as of the last load/save, used to compute the dirty flag.
    saved_text: String,
}

impl Document {
    pub fn untitled() -> Self {
        Self {
            path: None,
            text: String::new(),
            saved_text: String::new(),
        }
    }

    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            saved_text: text.clone(),
            text,
        })
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        let path = self.path.as_ref().context("document has no path")?;
        std::fs::write(path, &self.text)
            .with_context(|| format!("failed to write {}", path.display()))?;
        self.saved_text.clone_from(&self.text);
        Ok(())
    }

    pub fn save_as(&mut self, path: PathBuf) -> anyhow::Result<()> {
        self.path = Some(path);
        self.save()
    }

    pub fn is_dirty(&self) -> bool {
        self.text != self.saved_text
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
}

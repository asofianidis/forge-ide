//! Live Markdown preview beside the editor.

use std::path::{Path, PathBuf};

use egui::RichText;

use super::ForgeApp;
use crate::commands::Command;
use crate::editor::Document;
use crate::ui::icons::{self, Icon};
use crate::ui::markdown;

/// Extensions previewed as Markdown.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mkd"];

/// Whether `doc` is a Markdown file.
pub(super) fn is_markdown(doc: &Document) -> bool {
    let ext = doc.extension();
    MARKDOWN_EXTENSIONS
        .iter()
        .any(|m| ext.eq_ignore_ascii_case(m))
}

impl ForgeApp {
    /// Whether the active tab is a document (not a pane) in Markdown.
    pub(super) fn active_is_markdown(&self) -> bool {
        !self.pane_shown() && self.documents.get(self.active).is_some_and(is_markdown)
    }

    /// Whether the preview is showing next to the active document.
    pub(super) fn markdown_preview_open(&self) -> bool {
        self.active_is_markdown()
            && self
                .markdown_preview
                .contains(&self.documents[self.active].id)
    }

    pub(super) fn toggle_markdown_preview(&mut self) {
        if !self.active_is_markdown() {
            self.status = "Preview is available for Markdown files".to_owned();
            return;
        }
        let id = self.documents[self.active].id;
        if !self.markdown_preview.remove(&id) {
            self.markdown_preview.insert(id);
        }
    }

    /// A toggle for the preview, shown at the right end of the tab bar while
    /// a Markdown file is active.
    pub(super) fn markdown_preview_button(&mut self, ui: &mut egui::Ui) {
        if !self.active_is_markdown() {
            return;
        }
        let open = self.markdown_preview_open();
        let accent = self.current_theme().ui.accent.0;
        let mut tip = if open {
            "Hide Preview"
        } else {
            "Open Preview to the Side"
        }
        .to_owned();
        if let Some(s) = self.keymap.shortcut(Command::ToggleMarkdownPreview) {
            tip = format!("{tip} ({})", ui.ctx().format_shortcut(s));
        }
        if icons::button_tinted(ui, Icon::Preview, &tip, open.then_some(accent)).clicked() {
            self.toggle_markdown_preview();
        }
    }

    /// The rendered document in a resizable panel on the right of the editor.
    /// Call before laying out the editor so the editor takes the rest.
    pub(super) fn markdown_preview_panel(&mut self, ui: &mut egui::Ui) {
        if !self.markdown_preview_open() {
            return;
        }
        let width = ui.available_width();
        let doc = &self.documents[self.active];
        let mut clicked = None;
        let mut close = false;
        egui::Panel::right("markdown_preview")
            .resizable(true)
            .default_size(width / 2.0)
            .size_range(160.0..=(width - 160.0).max(160.0))
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Preview: {}", doc.title())).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close =
                            icons::button_sized(ui, Icon::Close, "Hide Preview", 18.0).clicked();
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt(("markdown_preview", doc.id))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        clicked = markdown::show_document(ui, doc.text());
                    });
            });
        if close {
            self.toggle_markdown_preview();
        }
        if let Some(target) = clicked {
            self.open_preview_link(&target);
        }
    }

    /// Opens a relative link clicked in the preview.
    fn open_preview_link(&mut self, target: &str) {
        let Some(doc_path) = self.documents[self.active].path.clone() else {
            return;
        };
        let root = self
            .source_control
            .root()
            .map(Path::to_path_buf)
            .or_else(|| self.workspace.as_ref().map(|w| w.root.path.clone()));
        match resolve_link(&doc_path, root.as_deref(), target) {
            Some(path) if path.is_file() => self.open_file(&path),
            Some(path) if path.is_dir() => {
                self.status = format!("{} is a folder", path.display());
            }
            Some(path) => self.status = format!("Not found: {}", path.display()),
            None if target.starts_with('#') => {
                self.status = "Links to headings aren't supported in the preview".to_owned();
            }
            None => self.status = format!("Can't open link: {target}"),
        }
    }
}

/// Resolves a link in the Markdown file at `doc_path` to a path on disk.
/// Relative links resolve against the file's folder, and `/`-rooted ones
/// against `root` (the repository), as on GitHub. Any `#fragment` or
/// `?query` is dropped and `%20`-style escapes are decoded.
fn resolve_link(doc_path: &Path, root: Option<&Path>, target: &str) -> Option<PathBuf> {
    let target = target.split(['#', '?']).next().unwrap_or_default();
    if target.is_empty() || markdown::is_web_link(target) || target.contains("://") {
        return None;
    }
    let (base, rel) = match target.strip_prefix('/') {
        Some(rest) => (root?.join("_"), rest),
        None => (doc_path.to_path_buf(), target),
    };
    let base = url::Url::from_file_path(&base).ok()?;
    base.join(rel).ok()?.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unix paths: file URLs need a drive letter on Windows.
    #[cfg(unix)]
    #[test]
    fn resolves_links_like_github() {
        let root = Path::new("/repo");
        let doc = Path::new("/repo/docs/guide.md");
        let r = |t| resolve_link(doc, Some(root), t);
        assert_eq!(r("intro.md"), Some("/repo/docs/intro.md".into()));
        assert_eq!(r("./img/a b.png"), Some("/repo/docs/img/a b.png".into()));
        assert_eq!(r("../LICENSE"), Some("/repo/LICENSE".into()));
        assert_eq!(
            r("my%20notes.md#usage"),
            Some("/repo/docs/my notes.md".into())
        );
        assert_eq!(r("/src/main.rs"), Some("/repo/src/main.rs".into()));
        assert_eq!(r("#usage"), None);
        assert_eq!(r("https://example.com"), None);
        assert_eq!(resolve_link(doc, None, "/src/main.rs"), None);
    }

    #[test]
    fn recognizes_markdown_files() {
        let mut doc = Document::untitled();
        assert!(!is_markdown(&doc));
        doc.path = Some("README.md".into());
        assert!(is_markdown(&doc));
        doc.path = Some("notes.MARKDOWN".into());
        assert!(is_markdown(&doc));
        doc.path = Some("main.rs".into());
        assert!(!is_markdown(&doc));
    }
}

//! Restoring the tabs that were open in a folder, and the recent folders list.

use std::path::{Path, PathBuf};

use super::ForgeApp;
use crate::editor::selection::{Cursor, Selections};
use crate::editor::view::Reveal;
use crate::settings::{OpenFile, Session, Settings};

/// Folder shown in menus, with the home directory abbreviated to `~`.
pub(super) fn display_folder(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}

/// The folder to show when Forge starts: the one given on the command line,
/// else the last folder opened, else the current directory.
pub fn startup_folder(arg: Option<PathBuf>, settings: &Settings) -> Option<PathBuf> {
    match arg {
        Some(path) => Some(path),
        None => settings
            .recent_folders
            .iter()
            .find(|f| f.is_dir())
            .cloned()
            .or_else(|| std::env::current_dir().ok()),
    }
}

impl ForgeApp {
    pub(super) fn update_settings(&mut self, change: impl FnOnce(&mut Settings)) {
        if let Err(e) = self.settings.update(change) {
            self.status = format!("Could not save settings: {e:#}");
        }
    }

    /// The open tabs of the current folder. Untitled buffers and files outside
    /// the folder aren't part of it.
    fn capture_session(&self, with_cursors: bool) -> Option<Session> {
        let folder = &self.workspace.as_ref()?.root.path;
        let mut files = Vec::new();
        let mut active = None;
        let pane_shown = self.pane_shown();
        // Tabs left to right across the groups, each file once, with the
        // cursor of its first view.
        let mut seen = Vec::new();
        for view in self.groups.iter().flat_map(|g| &g.tabs) {
            if seen.contains(&view.doc) {
                continue;
            }
            seen.push(view.doc);
            let Some(doc) = self.documents.iter().find(|d| d.id == view.doc) else {
                continue;
            };
            let Some(path) = doc.path.as_ref().filter(|p| p.starts_with(folder)) else {
                continue;
            };
            if self
                .documents
                .get(self.active)
                .is_some_and(|d| d.id == doc.id)
                && !pane_shown
            {
                active = Some(files.len());
            }
            let (line, column) = if with_cursors {
                doc.line_col(view.current_selection(&doc.buffer).primary().head)
            } else {
                (1, 1)
            };
            files.push(OpenFile {
                path: path.clone(),
                line,
                column,
            });
        }
        Some(Session {
            folder: folder.clone(),
            files,
            active,
        })
    }

    /// Saves the current folder's tabs. Unless `force`d, only writes when a
    /// tab was opened, closed or selected, so moving the cursor doesn't write
    /// the settings file every frame; cursors are saved on exit and when
    /// switching folders.
    pub(super) fn persist_session(&mut self, force: bool) {
        let Some(tabs) = self.capture_session(false) else {
            return;
        };
        if !force
            && self
                .saved_session
                .as_ref()
                .is_some_and(|s| s.same_tabs(&tabs))
        {
            return;
        }
        let Some(session) = self.capture_session(true) else {
            return;
        };
        self.saved_session = Some(session.clone());
        self.update_settings(|s| s.set_session(session));
    }

    /// Records a newly opened folder and reopens the tabs it had last time.
    pub(super) fn folder_opened(&mut self, folder: &Path) {
        self.start_watcher(folder);
        self.update_settings(|s| s.add_recent_folder(folder));
        let Some(session) = self.settings.session(folder).cloned() else {
            self.saved_session = None;
            return;
        };
        let mut restored = Vec::new();
        for file in &session.files {
            if !file.path.is_file() {
                continue;
            }
            self.open_file(&file.path);
            if let Some((doc, view)) = self.focused_doc_view()
                && doc.path.as_deref() == Some(file.path.as_path())
            {
                let idx = doc.char_index(file.line, file.column);
                view.set_selections(
                    &doc.buffer,
                    Selections::single(Cursor::caret(idx)),
                    Reveal::Center,
                );
                restored.push((doc.id, doc.line_col(idx)));
            }
        }
        let selected = session
            .active
            .and_then(|i| session.files.get(i))
            .and_then(|f| {
                restored.iter().find(|(id, _)| {
                    self.doc_by_id(*id)
                        .is_some_and(|i| self.documents[i].path.as_ref() == Some(&f.path))
                })
            })
            .or(restored.last());
        if let Some(&(id, line_col)) = selected
            && let Some(idx) = self.doc_by_id(id)
        {
            self.show_document(idx);
            self.cursor = line_col;
        }
        self.saved_session = Some(session);
        if !restored.is_empty() {
            let n = restored.len();
            self.status = format!(
                "Reopened {n} file{} from your last session",
                if n == 1 { "" } else { "s" }
            );
        }
    }

    /// Opens a folder from the recent list, dropping it if it's gone.
    pub(super) fn open_recent_folder(&mut self, folder: PathBuf) {
        if folder.is_dir() {
            self.open_folder(folder);
        } else {
            self.status = format!("{} no longer exists", display_folder(&folder));
            self.update_settings(|s| s.recent_folders.retain(|f| *f != folder));
        }
    }

    /// Recent folders other than the one that's open.
    pub(super) fn other_recent_folders(&self) -> Vec<PathBuf> {
        let current = self.workspace.as_ref().map(|w| &w.root.path);
        self.settings
            .recent_folders
            .iter()
            .filter(|f| Some(*f) != current)
            .cloned()
            .collect()
    }

    /// File → Open Recent.
    pub(super) fn open_recent_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Open Recent", |ui| {
            let folders = self.other_recent_folders();
            if folders.is_empty() {
                ui.add_enabled(false, egui::Button::new("No recent folders"));
                return;
            }
            for folder in folders {
                let name = folder.file_name().map_or_else(
                    || folder.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let button = egui::Button::new(name).shortcut_text(display_folder(&folder));
                if ui.add(button).clicked() {
                    self.open_recent_folder(folder);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Clear Recently Opened").clicked() {
                let keep: Vec<PathBuf> = self
                    .workspace
                    .as_ref()
                    .map(|w| w.root.path.clone())
                    .into_iter()
                    .collect();
                self.update_settings(|s| s.recent_folders = keep);
                ui.close();
            }
        });
    }
}

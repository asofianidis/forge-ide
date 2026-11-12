//! The Explorer sidebar: file tree, its context menu actions (new, rename,
//! delete, reveal, …) and keeping everything in step with the file watcher.

use std::path::{Path, PathBuf};

use egui::collapsing_header::CollapsingState;
use egui::text::{CCursor, CCursorRange};
use egui::{Id, RichText};

use super::ForgeApp;
use crate::commands::REVEAL_LABEL;
use crate::editor::DiskEvent;
use crate::git::{self, status::Change};
use crate::theme::UiColors;
use crate::ui::file_icons;
use crate::watcher::FsWatcher;
use crate::workspace::{self, FileNode};

/// Something picked in the file tree, carried out after it is drawn.
enum TreeAction {
    Open(PathBuf),
    History(PathBuf),
    /// Create a file (or folder) inside this directory.
    New {
        dir: PathBuf,
        folder: bool,
    },
    Rename(PathBuf),
    Delete(PathBuf),
    Reveal(PathBuf),
    CopyPath(PathBuf),
    CopyRelativePath(PathBuf),
}

/// A dialog opened from the file tree.
pub(super) enum ExplorerDialog {
    New {
        dir: PathBuf,
        folder: bool,
        name: String,
        error: Option<String>,
    },
    Rename {
        path: PathBuf,
        name: String,
        error: Option<String>,
    },
    Delete {
        path: PathBuf,
        /// Moving to the trash failed; offer deleting outright instead.
        trash_error: Option<String>,
    },
}

/// Persistent expanded/collapsed state of a folder in the tree.
fn folder_id(path: &Path) -> Id {
    Id::unique(("explorer", path))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl ForgeApp {
    pub(super) fn explorer(&mut self, ui: &mut egui::Ui) {
        let Some(ws) = &mut self.workspace else {
            if ui.button("Open Folder…").clicked() {
                self.open_folder_dialog();
            }
            return;
        };
        let mut action = None;
        let sc = &self.source_control;
        let colors = &self.themes[self.theme].ui;
        let decorate = |p: &Path| sc.decoration(p);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                file_tree(ui, &mut ws.root, true, &mut action, &decorate, colors)
            });
        if let Some(action) = action {
            self.tree_action(ui.ctx(), action);
        }
    }

    fn tree_action(&mut self, ctx: &egui::Context, action: TreeAction) {
        match action {
            TreeAction::Open(path) => self.open_file(&path),
            TreeAction::History(path) => {
                let rel = self.source_control.rel_path(&path);
                self.open_history(rel);
            }
            TreeAction::New { dir, folder } => {
                self.explorer_dialog = Some(ExplorerDialog::New {
                    dir,
                    folder,
                    name: String::new(),
                    error: None,
                });
            }
            TreeAction::Rename(path) => {
                // Select the name without its extension, ready to retype.
                let name = display_name(&path);
                let stem = match name.rfind('.') {
                    Some(i) if i > 0 && !path.is_dir() => name[..i].chars().count(),
                    _ => name.chars().count(),
                };
                let mut state =
                    egui::TextEdit::load_state(ctx, rename_field_id()).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(stem))));
                state.store(ctx, rename_field_id());
                self.explorer_dialog = Some(ExplorerDialog::Rename {
                    path,
                    name,
                    error: None,
                });
            }
            TreeAction::Delete(path) => {
                self.explorer_dialog = Some(ExplorerDialog::Delete {
                    path,
                    trash_error: None,
                });
            }
            TreeAction::Reveal(path) => self.reveal(&path),
            TreeAction::CopyPath(path) => {
                ctx.copy_text(path.display().to_string());
                self.status = "Copied path".to_owned();
            }
            TreeAction::CopyRelativePath(path) => {
                let root = self.workspace.as_ref().map(|w| w.root.path.as_path());
                let rel = root
                    .and_then(|r| path.strip_prefix(r).ok())
                    .unwrap_or(&path);
                ctx.copy_text(rel.to_string_lossy().replace('\\', "/"));
                self.status = "Copied relative path".to_owned();
            }
        }
    }

    pub(super) fn reveal(&mut self, path: &Path) {
        if let Err(e) = reveal_in_file_manager(path) {
            self.status = format!("Error: couldn't reveal {}: {e:#}", path.display());
        }
    }

    /// Re-reads a folder in the tree right away, without waiting for the
    /// watcher, and re-checks git status.
    pub(super) fn reload_tree_dir(&mut self, dir: &Path) {
        if let Some(node) = self.workspace.as_mut().and_then(|ws| ws.root.find_mut(dir)) {
            node.reload();
        }
        self.source_control.files_changed(false);
    }

    /// Shows the New / Rename / Delete dialog and carries it out.
    pub(super) fn explorer_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.explorer_dialog else {
            return;
        };
        let mut submit = false;
        let mut permanent = false;
        let mut cancel = false;
        let is_rename = matches!(dialog, ExplorerDialog::Rename { .. });
        let modal = egui::Modal::new(Id::unique("explorer_dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            let (heading, name, error, hint) = match dialog {
                ExplorerDialog::New {
                    dir,
                    folder,
                    name,
                    error,
                } => (
                    if *folder { "New Folder" } else { "New File" }.to_owned(),
                    name,
                    error,
                    format!(
                        "Inside {}. Use / to create it in new folders.",
                        display_name(dir)
                    ),
                ),
                ExplorerDialog::Rename { path, name, error } => (
                    format!("Rename {}", display_name(path)),
                    name,
                    error,
                    String::new(),
                ),
                ExplorerDialog::Delete { path, trash_error } => {
                    let what = if path.is_dir() {
                        "folder and its contents"
                    } else {
                        "file"
                    };
                    ui.heading(format!("Delete {}?", display_name(path)));
                    match trash_error {
                        None => {
                            ui.label(format!("The {what} will be moved to the trash."));
                        }
                        Some(e) => {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                format!("Couldn't move it to the trash: {e}"),
                            );
                            ui.label(format!(
                                "Delete the {what} permanently instead? This can't be undone."
                            ));
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if trash_error.is_none() {
                            submit = ui.button("Move to Trash").clicked();
                        } else {
                            permanent = ui.button("Delete Permanently").clicked();
                        }
                        cancel = ui.button("Cancel").clicked();
                    });
                    return;
                }
            };
            ui.heading(heading);
            let id = if is_rename {
                rename_field_id()
            } else {
                Id::unique("explorer_new_field")
            };
            let response = ui.add(
                egui::TextEdit::singleline(name)
                    .id(id)
                    .desired_width(f32::INFINITY),
            );
            if !response.has_focus() && !response.lost_focus() {
                response.request_focus();
            }
            if response.changed() {
                *error = None;
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            match error {
                Some(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                }
                None if !hint.is_empty() => {
                    ui.weak(hint.as_str());
                }
                None => {}
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                submit |= ui.button("OK").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if cancel || (modal.should_close() && !submit && !permanent) {
            self.explorer_dialog = None;
            return;
        }
        if !submit && !permanent {
            return;
        }
        let Some(dialog) = self.explorer_dialog.take() else {
            return;
        };
        match dialog {
            ExplorerDialog::New {
                dir, folder, name, ..
            } => match workspace::create_entry(&dir, &name, folder) {
                Ok(path) => {
                    // Show the new entry: expand every folder down to it.
                    let mut open = path.parent();
                    while let Some(d) = open.filter(|d| d.starts_with(&dir)) {
                        let mut state =
                            CollapsingState::load_with_default_open(ctx, folder_id(d), false);
                        state.set_open(true);
                        state.store(ctx);
                        open = d.parent();
                    }
                    self.reload_tree_dir(&dir);
                    if folder {
                        self.status = format!("Created {}", path.display());
                    } else {
                        self.open_file(&path);
                    }
                }
                Err(e) => {
                    self.explorer_dialog = Some(ExplorerDialog::New {
                        dir,
                        folder,
                        name,
                        error: Some(format!("{e:#}")),
                    });
                }
            },
            ExplorerDialog::Rename { path, name, .. } => {
                match workspace::rename_entry(&path, &name) {
                    Ok(target) => {
                        self.moved(&path, &target);
                        if let Some(dir) = path.parent() {
                            self.reload_tree_dir(dir);
                        }
                        self.status = format!("Renamed to {}", display_name(&target));
                    }
                    Err(e) => {
                        self.explorer_dialog = Some(ExplorerDialog::Rename {
                            path,
                            name,
                            error: Some(format!("{e:#}")),
                        });
                    }
                }
            }
            ExplorerDialog::Delete { path, .. } => {
                let result = if permanent {
                    if path.is_dir() {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    }
                    .map_err(|e| e.to_string())
                } else {
                    trash::delete(&path).map_err(|e| e.to_string())
                };
                match result {
                    Ok(()) => {
                        self.deleted(&path);
                        if let Some(dir) = path.parent() {
                            self.reload_tree_dir(dir);
                        }
                        self.status = if permanent {
                            format!("Deleted {}", display_name(&path))
                        } else {
                            format!("Moved {} to the trash", display_name(&path))
                        };
                    }
                    Err(e) if permanent => {
                        self.status = format!("Error: couldn't delete {}: {e}", path.display());
                    }
                    Err(e) => {
                        self.explorer_dialog = Some(ExplorerDialog::Delete {
                            path,
                            trash_error: Some(e),
                        });
                    }
                }
            }
        }
    }

    /// Points open tabs at their new location after a rename.
    fn moved(&mut self, from: &Path, to: &Path) {
        for doc in &mut self.documents {
            if let Some(new) = doc
                .path
                .as_deref()
                .and_then(|p| workspace::renamed_path(p, from, to))
            {
                doc.path = Some(new);
            }
        }
    }

    /// Closes tabs for deleted files. Tabs with unsaved changes stay open
    /// (marked as deleted) so nothing is lost.
    fn deleted(&mut self, path: &Path) {
        let mut i = self.documents.len();
        while i > 0 {
            i -= 1;
            let doc = &mut self.documents[i];
            if !doc.path.as_deref().is_some_and(|p| p.starts_with(path)) {
                continue;
            }
            doc.poll_disk();
            if !doc.has_unsaved_edits() {
                self.close_document(i);
            }
        }
    }

    /// (Re)starts watching the workspace folder for outside changes.
    pub(super) fn start_watcher(&mut self, folder: &Path) {
        self.watcher = match FsWatcher::new(folder, &self.egui_ctx) {
            Ok(w) => Some(w),
            Err(e) => {
                // Open files are still polled; the tree has Refresh.
                self.status = format!("File watching unavailable: {e}");
                None
            }
        };
    }

    /// Applies outside changes reported by the watcher to the tree, open
    /// files and git status.
    pub(super) fn poll_watcher(&mut self, ctx: &egui::Context) {
        let Some(changes) = self.watcher.as_mut().and_then(|w| w.poll(ctx)) else {
            return;
        };
        if let Some(ws) = &mut self.workspace {
            if changes.rescan {
                ws.root.reload_all();
            } else {
                for dir in changes.parent_dirs() {
                    if let Some(node) = ws.root.find_mut(dir) {
                        node.reload();
                    }
                }
            }
        }
        let editorconfig_changed = changes.rescan
            || changes
                .paths
                .iter()
                .any(|p| p.file_name() == Some(crate::editorconfig::FILE_NAME.as_ref()));
        for doc in &mut self.documents {
            if editorconfig_changed {
                doc.refresh_editorconfig();
            }
            if !doc.path.as_deref().is_some_and(|p| changes.touches(p)) {
                continue;
            }
            match doc.poll_disk() {
                Some(DiskEvent::Reloaded) => {
                    self.status = format!("Reloaded {} (changed on disk)", doc.title());
                }
                Some(DiskEvent::Conflict) => {
                    self.status = format!("{} changed on disk", doc.title());
                }
                Some(DiskEvent::Deleted) => {
                    self.status = format!("{} was deleted on disk", doc.title());
                }
                None => {}
            }
        }
        if changes.git || changes.rescan || !changes.paths.is_empty() {
            self.source_control
                .files_changed(changes.git || changes.rescan);
        }
    }

    /// Whether the watcher covers `path`, so it needn't be polled.
    pub(super) fn is_watched(&self, path: &Path) -> bool {
        self.watcher
            .as_ref()
            .is_some_and(|w| path.starts_with(w.root()))
    }
}

fn rename_field_id() -> Id {
    Id::unique("explorer_rename_field")
}

/// Shows `path` selected in the platform's file manager.
fn reveal_in_file_manager(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        std::process::Command::new("explorer").arg(arg).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Ask the file manager to select the item; fall back to opening the
        // folder that contains it.
        let uri = file_uri(path);
        let selected = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            .arg(format!("array:string:{uri}"))
            .arg("string:")
            .status()
            .is_ok_and(|s| s.success());
        if selected {
            return Ok(());
        }
        super::open_path(path.parent().unwrap_or(path))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// Context menu entries shared by files and folders.
fn common_menu(ui: &mut egui::Ui, path: &Path, is_root: bool, action: &mut Option<TreeAction>) {
    let mut pick = |ui: &mut egui::Ui, label: &str, a: TreeAction| {
        if ui.button(label).clicked() {
            *action = Some(a);
            ui.close();
        }
    };
    ui.separator();
    pick(ui, REVEAL_LABEL, TreeAction::Reveal(path.to_path_buf()));
    pick(ui, "Copy Path", TreeAction::CopyPath(path.to_path_buf()));
    pick(
        ui,
        "Copy Relative Path",
        TreeAction::CopyRelativePath(path.to_path_buf()),
    );
    ui.separator();
    pick(ui, "Show History", TreeAction::History(path.to_path_buf()));
    if !is_root {
        ui.separator();
        pick(ui, "Rename…", TreeAction::Rename(path.to_path_buf()));
        pick(ui, "Delete…", TreeAction::Delete(path.to_path_buf()));
    }
}

fn new_menu(ui: &mut egui::Ui, dir: &Path, action: &mut Option<TreeAction>) {
    for (label, folder) in [("New File…", false), ("New Folder…", true)] {
        if ui.button(label).clicked() {
            *action = Some(TreeAction::New {
                dir: dir.to_path_buf(),
                folder,
            });
            ui.close();
        }
    }
}

/// Recursively renders a directory tree. Names are colored by their git
/// state; picking something sets `action`.
fn file_tree(
    ui: &mut egui::Ui,
    node: &mut FileNode,
    is_root: bool,
    action: &mut Option<TreeAction>,
    decorate: &dyn Fn(&Path) -> Option<Change>,
    colors: &UiColors,
) {
    let change = decorate(&node.path);
    let mut name = RichText::new(&node.name);
    if let Some(c) = change {
        name = name.color(git::panel_change_color(c, colors));
        if c == Change::Deleted {
            name = name.strikethrough();
        }
    }
    let dark = ui.visuals().dark_mode;
    if node.is_dir {
        let id = folder_id(&node.path);
        let state = CollapsingState::load_with_default_open(ui.ctx(), id, is_root);
        let open = state.is_open();
        let (_, header, _) = state
            .show_header(ui, |ui| {
                file_icons::show(ui, file_icons::folder(&node.name, open, is_root, dark));
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add(egui::Button::selectable(false, name).frame_when_inactive(false))
            })
            .body(|ui| {
                for child in node.children() {
                    file_tree(ui, child, false, action, decorate, colors);
                }
            });
        // Clicking the name toggles the folder, like the arrow does.
        if header.inner.clicked() {
            let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, is_root);
            state.toggle(ui);
            state.store(ui.ctx());
        }
        header.inner.context_menu(|ui| {
            new_menu(ui, &node.path, action);
            if ui.button("Refresh").clicked() {
                node.reload_all();
                ui.close();
            }
            common_menu(ui, &node.path, is_root, action);
        });
    } else {
        let response = ui
            .horizontal(|ui| {
                // Leave room for the arrow folders have, so icons line up.
                ui.add_space(ui.spacing().indent);
                file_icons::show(ui, file_icons::file(&node.name, dark));
                ui.spacing_mut().item_spacing.x = 4.0;
                let r = ui.selectable_label(false, name);
                if let Some(c) = change {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(c.letter())
                                .small()
                                .color(git::panel_change_color(c, colors)),
                        )
                        .on_hover_text(c.describe());
                    });
                }
                r
            })
            .inner;
        if response.clicked() {
            *action = Some(TreeAction::Open(node.path.clone()));
        }
        response.context_menu(|ui| {
            if ui.button("Open").clicked() {
                *action = Some(TreeAction::Open(node.path.clone()));
                ui.close();
            }
            ui.separator();
            if let Some(dir) = node.path.parent() {
                new_menu(ui, dir, action);
            }
            common_menu(ui, &node.path, false, action);
        });
    }
}

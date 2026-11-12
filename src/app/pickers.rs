//! Quick open (Go to File), the command palette and the theme picker.

use std::path::PathBuf;

use super::ForgeApp;
use super::session::display_folder;
use crate::commands::Command;
use crate::fuzzy::{self, FuzzyMatch};
use crate::palette::{MAX_ROWS, PickerItem, PickerStyle};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum PickerMode {
    #[default]
    Files,
    Commands,
    Themes,
    RecentFolders,
    /// Definitions, references or problems from the language server.
    Locations,
}

/// A command palette entry.
#[derive(Debug, Clone)]
enum PaletteCommand {
    Builtin(Command),
    /// An extension command by qualified id.
    Extension(String),
}

/// Fuzzy-matched file list, cached until the query or the index changes.
#[derive(Default)]
pub(super) struct FileMatches {
    key: Option<(String, usize)>,
    matches: Vec<(usize, FuzzyMatch)>,
    total: usize,
}

/// Sorts `(index, match)` pairs best-first, keeping the original order among
/// equal scores.
fn rank(mut matches: Vec<(usize, FuzzyMatch)>) -> Vec<(usize, FuzzyMatch)> {
    matches.sort_by(|a, b| b.1.score.cmp(&a.1.score).then(a.0.cmp(&b.0)));
    matches
}

impl ForgeApp {
    pub(super) fn open_quick_open(&mut self, ctx: &egui::Context) {
        let Some(ws) = &self.workspace else {
            self.status = "Open a folder to search its files".to_owned();
            return;
        };
        let root = ws.root.path.clone();
        self.file_index.refresh(&root, ctx);
        self.go_to_line = None;
        self.picker_mode = PickerMode::Files;
        self.picker.open("");
    }

    pub(super) fn open_command_palette(&mut self) {
        self.go_to_line = None;
        self.picker_mode = PickerMode::Commands;
        self.picker.open("");
    }

    pub(super) fn open_theme_picker(&mut self) {
        self.go_to_line = None;
        self.picker_mode = PickerMode::Themes;
        self.picker.open("");
    }

    pub(super) fn open_locations_picker(&mut self) {
        self.go_to_line = None;
        self.picker_mode = PickerMode::Locations;
        self.picker.open("");
    }

    pub(super) fn open_recent_picker(&mut self) {
        if self.other_recent_folders().is_empty() {
            self.status = "No recent folders".to_owned();
            return;
        }
        self.go_to_line = None;
        self.picker_mode = PickerMode::RecentFolders;
        self.picker.open("");
    }

    fn recent_folder_items(&self) -> (Vec<PickerItem>, Vec<PathBuf>) {
        let folders = self.other_recent_folders();
        let labels: Vec<String> = folders.iter().map(|f| display_folder(f)).collect();
        let matches: Vec<_> = labels
            .iter()
            .enumerate()
            .filter_map(|(i, l)| fuzzy::score(&self.picker.query, l).map(|m| (i, m)))
            .collect();
        // Keep most-recent-first order until the user types something.
        let ranked = if self.picker.query.trim().is_empty() {
            matches
        } else {
            rank(matches)
        };
        ranked
            .into_iter()
            .map(|(i, m)| {
                (
                    PickerItem {
                        label: labels[i].clone(),
                        detail: String::new(),
                        highlights: m.indices,
                    },
                    folders[i].clone(),
                )
            })
            .unzip()
    }

    fn command_items(&self, ctx: &egui::Context) -> (Vec<PickerItem>, Vec<PaletteCommand>) {
        // Extension commands that apply to the active tab.
        let file_name = self
            .active_pane
            .is_none()
            .then(|| self.documents.get(self.active))
            .flatten()
            .and_then(|d| d.path.as_deref())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let extension_commands = self
            .extensions
            .commands()
            .filter(|(_, c, _)| c.applies_to(file_name.as_deref()))
            .map(|(e, c, id)| {
                let detail = c
                    .shortcut()
                    .map(|s| ctx.format_shortcut(&s))
                    .unwrap_or_default();
                (
                    PaletteCommand::Extension(id),
                    format!("{}: {}", e.name(), c.title),
                    detail,
                )
            });
        let candidates: Vec<(PaletteCommand, String, String)> = Command::ALL
            .iter()
            .filter(|&&c| c != Command::ShowCommandPalette)
            .map(|&c| {
                let detail = self
                    .keymap
                    .shortcut(c)
                    .map(|s| ctx.format_shortcut(s))
                    .unwrap_or_default();
                (PaletteCommand::Builtin(c), c.palette_label(), detail)
            })
            .chain(extension_commands)
            .collect();
        let ranked = rank(
            candidates
                .iter()
                .enumerate()
                .filter_map(|(i, (_, label, _))| {
                    fuzzy::score(&self.picker.query, label).map(|m| (i, m))
                })
                .collect(),
        );
        ranked
            .into_iter()
            .map(|(i, m)| {
                let (cmd, label, detail) = &candidates[i];
                (
                    PickerItem {
                        label: label.clone(),
                        detail: detail.clone(),
                        highlights: m.indices,
                    },
                    cmd.clone(),
                )
            })
            .unzip()
    }

    fn theme_items(&self) -> (Vec<PickerItem>, Vec<usize>) {
        let ranked = rank(
            self.themes
                .iter()
                .enumerate()
                .filter_map(|(i, t)| fuzzy::score(&self.picker.query, &t.name).map(|m| (i, m)))
                .collect(),
        );
        ranked
            .into_iter()
            .map(|(i, m)| {
                let detail = if i == self.theme { "current" } else { "" };
                (
                    PickerItem {
                        label: self.themes[i].name.clone(),
                        detail: detail.to_owned(),
                        highlights: m.indices,
                    },
                    i,
                )
            })
            .unzip()
    }

    fn refresh_file_matches(&mut self) {
        let key = (self.picker.query.clone(), self.file_index_generation);
        if self.file_matches.key.as_ref() == Some(&key) {
            return;
        }
        let files = &self.file_index.files;
        let matches: Vec<(usize, FuzzyMatch)> = files
            .iter()
            .enumerate()
            .filter_map(|(i, f)| fuzzy::score(&self.picker.query, f).map(|m| (i, m)))
            .collect();
        let total = matches.len();
        let mut ranked = if self.picker.query.trim().is_empty() {
            matches
        } else {
            rank(matches)
        };
        ranked.truncate(MAX_ROWS);
        self.file_matches = FileMatches {
            key: Some(key),
            matches: ranked,
            total,
        };
    }

    fn file_items(&self) -> Vec<PickerItem> {
        self.file_matches
            .matches
            .iter()
            .map(|(i, m)| PickerItem {
                label: self.file_index.files[*i].clone(),
                detail: String::new(),
                highlights: m.indices.clone(),
            })
            .collect()
    }

    /// Draws whichever picker is open and acts on the chosen item.
    pub(super) fn show_picker(&mut self, ctx: &egui::Context) {
        if self.file_index.poll() {
            self.file_index_generation += 1;
        }
        if !self.picker.open {
            return;
        }
        let theme = &self.current_theme().ui;
        let style = PickerStyle {
            highlight: theme.accent.0,
            muted: theme.text_muted.0,
        };
        match self.picker_mode {
            PickerMode::Commands => {
                let (items, commands) = self.command_items(ctx);
                if let Some(i) = self
                    .picker
                    .show(ctx, "Type a command", &items, None, &style)
                {
                    match &commands[i] {
                        PaletteCommand::Builtin(c) => self.pending_commands.push(*c),
                        PaletteCommand::Extension(id) => self.ext.pending.push(id.clone()),
                    }
                }
            }
            PickerMode::Themes => {
                let (items, themes) = self.theme_items();
                if let Some(i) = self
                    .picker
                    .show(ctx, "Select a color theme", &items, None, &style)
                {
                    self.set_theme(ctx, themes[i]);
                }
            }
            PickerMode::Locations => {
                let (items, indices) = self.location_picker_items(&self.picker.query);
                let hint = self.language.locations_hint;
                if let Some(i) = self.picker.show(ctx, hint, &items, None, &style) {
                    let location = self.language.locations[indices[i]].location.clone();
                    self.go_to_location(ctx, &location);
                }
            }
            PickerMode::RecentFolders => {
                let (items, folders) = self.recent_folder_items();
                if let Some(i) = self
                    .picker
                    .show(ctx, "Open a recent folder", &items, None, &style)
                {
                    self.open_recent_folder(folders[i].clone());
                }
            }
            PickerMode::Files => {
                self.refresh_file_matches();
                let items = self.file_items();
                let footer = if self.file_index.is_loading() && self.file_index.files.is_empty() {
                    "Indexing files…".to_owned()
                } else if self.file_matches.total > items.len() {
                    format!(
                        "Showing {} of {} matching files",
                        items.len(),
                        self.file_matches.total
                    )
                } else {
                    let n = self.file_index.files.len();
                    format!("{n} files")
                };
                if let Some(i) =
                    self.picker
                        .show(ctx, "Search files by name", &items, Some(&footer), &style)
                {
                    let (file, _) = &self.file_matches.matches[i];
                    if let Some(root) = &self.file_index.root {
                        let path = root.join(&self.file_index.files[*file]);
                        self.open_file(&path);
                        if let Some(id) = self.active_editor_id() {
                            ctx.memory_mut(|m| m.request_focus(id));
                        }
                    }
                }
            }
        }
    }
}

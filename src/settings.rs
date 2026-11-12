//! Persisted user preferences, stored in `<config dir>/forge-ide/settings.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::theme::config_dir;

/// Folders kept in File → Open Recent.
pub const MAX_RECENT_FOLDERS: usize = 10;
/// Folders whose open tabs are remembered.
const MAX_SESSIONS: usize = 30;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name of the selected theme.
    pub theme: Option<String>,
    /// Check for updates on startup. Defaults to on for release builds.
    pub auto_update: Option<bool>,
    /// Recently opened folders, most recent first.
    pub recent_folders: Vec<PathBuf>,
    /// Open tabs per folder, most recently used first.
    pub sessions: Vec<Session>,
}

/// The tabs that were open in a folder, restored when it is opened again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub folder: PathBuf,
    pub files: Vec<OpenFile>,
    /// Index into `files` of the selected tab.
    pub active: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    pub path: PathBuf,
    /// 1-based cursor position.
    pub line: usize,
    pub column: usize,
}

impl Session {
    /// Same files in the same order with the same selected tab (ignores cursors).
    pub fn same_tabs(&self, other: &Session) -> bool {
        self.folder == other.folder
            && self.active == other.active
            && self.files.len() == other.files.len()
            && self
                .files
                .iter()
                .zip(&other.files)
                .all(|(a, b)| a.path == b.path)
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("settings.toml"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<()> {
        let path = Self::path().context("no config directory")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", path.display()))
    }

    /// Applies `change` to the settings currently on disk and saves them.
    ///
    /// Re-reading first keeps changes made by other Forge windows (e.g. their
    /// sessions) instead of overwriting them with this window's copy.
    pub fn update(&mut self, change: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut fresh = Self::load();
        change(&mut fresh);
        let saved = fresh.save();
        *self = fresh;
        saved
    }

    /// Moves `folder` to the front of the recent folders list.
    pub fn add_recent_folder(&mut self, folder: &Path) {
        self.recent_folders.retain(|f| f != folder);
        self.recent_folders.insert(0, folder.to_path_buf());
        self.recent_folders.truncate(MAX_RECENT_FOLDERS);
    }

    pub fn session(&self, folder: &Path) -> Option<&Session> {
        self.sessions.iter().find(|s| s.folder == folder)
    }

    /// Stores `session`, replacing any earlier one for the same folder.
    pub fn set_session(&mut self, session: Session) {
        self.sessions.retain(|s| s.folder != session.folder);
        self.sessions.insert(0, session);
        self.sessions.truncate(MAX_SESSIONS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_default() {
        let s: Settings = toml::from_str("theme = \"Blossom\"").unwrap();
        assert_eq!(s.theme.as_deref(), Some("Blossom"));
        assert_eq!(s.auto_update, None);
        assert!(s.recent_folders.is_empty());
        let s: Settings = toml::from_str("").unwrap();
        assert!(s.theme.is_none());
    }

    #[test]
    fn recent_folders_dedupe_and_cap() {
        let mut s = Settings::default();
        for i in 0..15 {
            s.add_recent_folder(Path::new(&format!("/p{i}")));
        }
        s.add_recent_folder(Path::new("/p5"));
        assert_eq!(s.recent_folders.len(), MAX_RECENT_FOLDERS);
        assert_eq!(s.recent_folders[0], Path::new("/p5"));
        assert_eq!(
            s.recent_folders
                .iter()
                .filter(|f| *f == Path::new("/p5"))
                .count(),
            1
        );
    }

    #[test]
    fn sessions_round_trip_through_toml() {
        let mut s = Settings {
            theme: Some("Blossom".into()),
            ..Settings::default()
        };
        s.add_recent_folder(Path::new("/code/a"));
        s.set_session(Session {
            folder: "/code/a".into(),
            files: vec![OpenFile {
                path: "/code/a/main.rs".into(),
                line: 12,
                column: 4,
            }],
            active: Some(0),
        });
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.recent_folders, s.recent_folders);
        assert_eq!(back.sessions, s.sessions);
        assert_eq!(back.theme, s.theme);
    }

    #[test]
    fn set_session_replaces_same_folder() {
        let mut s = Settings::default();
        let session = |folder: &str, active| Session {
            folder: folder.into(),
            files: Vec::new(),
            active,
        };
        s.set_session(session("/a", None));
        s.set_session(session("/b", None));
        s.set_session(session("/a", Some(0)));
        assert_eq!(s.sessions.len(), 2);
        assert_eq!(s.sessions[0], session("/a", Some(0)));
        assert_eq!(s.session(Path::new("/b")), Some(&session("/b", None)));
    }
}

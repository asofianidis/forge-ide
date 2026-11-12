//! Persisted user preferences, stored in `<config dir>/forge-ide/settings.toml`
//! (or the file named by `FORGE_SETTINGS`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::keymap::Keys;
use crate::theme::config_dir;

/// Folders kept in File → Open Recent.
pub const MAX_RECENT_FOLDERS: usize = 10;
/// Folders whose open tabs are remembered.
const MAX_SESSIONS: usize = 30;

/// Editor font size range and default, in points.
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;
pub const DEFAULT_FONT_SIZE: f32 = 14.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name of the selected theme.
    pub theme: Option<String>,
    /// Check for updates on startup. Defaults to on for release builds.
    pub auto_update: Option<bool>,
    /// Format files with their language server when saving.
    pub format_on_save: bool,
    /// Editor font size in points, kept within [`MIN_FONT_SIZE`]..=[`MAX_FONT_SIZE`].
    pub font_size: f32,
    /// Wrap long lines at the editor's width instead of scrolling sideways.
    pub word_wrap: bool,
    /// Show line numbers in the editor gutter.
    pub line_numbers: bool,
    /// Show a zoomed-out overview of the file beside the editor.
    pub minimap: bool,
    /// Shell for the integrated terminal, with optional arguments (e.g. `"bash --login"`).
    /// Defaults to PowerShell on Windows and `$SHELL` elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_shell: Option<String>,
    /// Ids of installed extensions that are switched off.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_extensions: Vec<String>,
    /// Shortcuts by command id (`toggle_terminal = "cmd+j"`), replacing the
    /// defaults. See [`crate::keymap`].
    pub keybindings: BTreeMap<String, Keys>,
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

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: None,
            auto_update: None,
            format_on_save: false,
            font_size: DEFAULT_FONT_SIZE,
            word_wrap: false,
            line_numbers: true,
            minimap: true,
            terminal_shell: None,
            disabled_extensions: Vec::new(),
            keybindings: BTreeMap::new(),
            recent_folders: Vec::new(),
            sessions: Vec::new(),
        }
    }
}

/// `size` limited to the supported range; anything unusable becomes the default.
pub fn clamp_font_size(size: f32) -> f32 {
    if size.is_finite() {
        size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    } else {
        DEFAULT_FONT_SIZE
    }
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
    /// `$FORGE_SETTINGS` if set, otherwise `<config dir>/forge-ide/settings.toml`.
    pub fn path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("FORGE_SETTINGS").filter(|p| !p.is_empty()) {
            return Some(PathBuf::from(path));
        }
        config_dir().map(|d| d.join("settings.toml"))
    }

    /// The settings on disk, or the defaults if there are none or they can't be read.
    pub fn load() -> Self {
        Self::try_load().unwrap_or_default()
    }

    /// The settings on disk. A missing file (or config directory) gives the
    /// defaults; a file that can't be read or parsed is an error.
    pub fn try_load() -> Result<Self> {
        match Self::path() {
            Some(path) => Self::load_from(&path),
            None => Ok(Self::default()),
        }
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .with_context(|| format!("invalid settings in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    /// Parses settings TOML. Missing fields get their defaults.
    pub fn parse(text: &str) -> Result<Self> {
        let mut settings: Self = toml::from_str(text)?;
        settings.font_size = clamp_font_size(settings.font_size);
        Ok(settings)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", path.display()))
    }

    /// Applies `change` to the settings currently on disk and saves them.
    ///
    /// Re-reading first keeps changes made by other Forge windows (e.g. their
    /// sessions) instead of overwriting them with this window's copy. If the
    /// file on disk is invalid (say, mid-edit by hand), the change is applied
    /// to this window's copy only and the file is left alone.
    pub fn update(&mut self, change: impl FnOnce(&mut Settings)) -> Result<()> {
        let path = Self::path().context("no config directory")?;
        match Self::load_from(&path) {
            Ok(mut fresh) => {
                change(&mut fresh);
                let saved = fresh.save_to(&path);
                *self = fresh;
                saved
            }
            Err(e) => {
                change(self);
                Err(e.context("not saved"))
            }
        }
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
        assert_eq!(s.terminal_shell, None);
        let s: Settings = toml::from_str("").unwrap();
        assert!(s.theme.is_none());
    }

    #[test]
    fn editor_settings_default_when_missing() {
        let s = Settings::parse("theme = \"Blossom\"\nformat_on_save = true").unwrap();
        assert_eq!(s.font_size, DEFAULT_FONT_SIZE);
        assert!(!s.word_wrap);
        assert!(s.line_numbers);
        assert!(s.minimap);
        assert!(s.keybindings.is_empty());
        assert!(s.format_on_save);
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
        let s = Settings::parse("word_wrap = true\nline_numbers = false").unwrap();
        assert!(s.word_wrap);
        assert!(!s.line_numbers);
        assert_eq!(s.font_size, DEFAULT_FONT_SIZE);
    }

    #[test]
    fn font_size_is_clamped() {
        let size = |text: &str| Settings::parse(text).unwrap().font_size;
        assert_eq!(size("font_size = 1000"), MAX_FONT_SIZE);
        assert_eq!(size("font_size = 1"), MIN_FONT_SIZE);
        assert_eq!(size("font_size = -4.5"), MIN_FONT_SIZE);
        assert_eq!(size("font_size = 17"), 17.0);
        assert_eq!(size("font_size = nan"), DEFAULT_FONT_SIZE);
        assert_eq!(size("font_size = inf"), DEFAULT_FONT_SIZE);
    }

    #[test]
    fn rejects_malformed_toml() {
        assert!(Settings::parse("font_size = \"big\"").is_err());
        assert!(Settings::parse("word_wrap = ").is_err());
    }

    #[test]
    fn load_and_save_file() {
        let dir = std::env::temp_dir().join(format!("forge-settings-{}", std::process::id()));
        let path = dir.join("nested").join("settings.toml");
        assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
        let s = Settings {
            font_size: 18.0,
            word_wrap: true,
            line_numbers: false,
            ..Settings::default()
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), s);
        std::fs::write(&path, "font_size = [").unwrap();
        assert!(Settings::load_from(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
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
            auto_update: Some(false),
            format_on_save: true,
            font_size: 16.0,
            word_wrap: true,
            line_numbers: false,
            minimap: false,
            terminal_shell: Some("bash --login".into()),
            keybindings: [
                ("save".to_owned(), Keys::One("cmd+s".into())),
                (
                    "find".to_owned(),
                    Keys::Many(vec!["cmd+f".into(), "f3".into()]),
                ),
            ]
            .into(),
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
        assert_eq!(Settings::parse(&text).unwrap(), s);
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

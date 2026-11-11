//! Persisted user preferences, stored in `<config dir>/forge-ide/settings.toml`.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::theme::config_dir;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name of the selected theme.
    pub theme: Option<String>,
    /// Check for updates on startup. Defaults to on for release builds.
    pub auto_update: Option<bool>,
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

    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("no config directory")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", path.display()))
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
        let s: Settings = toml::from_str("").unwrap();
        assert!(s.theme.is_none());
    }
}

//! User settings, persisted as TOML in the platform config directory.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

/// Syntax theme used when none is configured (or the configured one is unknown).
pub const DEFAULT_THEME: &str = "base16-ocean.dark";

pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Settings {
    /// Syntax highlighting theme; the UI switches to light or dark to match it.
    pub theme: String,
    /// Editor font size in points.
    pub font_size: f32,
    pub word_wrap: bool,
    pub line_numbers: bool,
    /// Shell for the integrated terminal, with optional arguments (e.g. `"bash --login"`).
    /// Defaults to PowerShell on Windows and `$SHELL` elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_shell: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME.to_owned(),
            font_size: 14.0,
            word_wrap: false,
            line_numbers: true,
            terminal_shell: None,
        }
    }
}

impl Settings {
    /// `$FORGE_SETTINGS` if set, otherwise `<config dir>/forge-ide/settings.toml`
    /// (e.g. `%APPDATA%\forge-ide\settings.toml` on Windows).
    pub fn default_path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("FORGE_SETTINGS").filter(|p| !p.is_empty()) {
            return Some(PathBuf::from(path));
        }
        dirs::config_dir().map(|d| d.join("forge-ide").join("settings.toml"))
    }

    /// Loads settings from `path`. A missing file yields the defaults.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .with_context(|| format!("invalid settings in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut settings: Self = toml::from_str(text)?;
        settings.font_size = settings.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        Ok(settings)
    }

    /// Editor controls for the settings window. Returns true if anything changed.
    pub fn ui(&mut self, ui: &mut egui::Ui, theme_names: &[String]) -> bool {
        let before = self.clone();
        egui::Grid::new("settings_grid")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("Color theme");
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(&self.theme)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for name in theme_names {
                            ui.selectable_value(&mut self.theme, name.clone(), name);
                        }
                    });
                ui.end_row();

                ui.label("Font size");
                ui.add(
                    egui::Slider::new(&mut self.font_size, MIN_FONT_SIZE..=MAX_FONT_SIZE)
                        .step_by(1.0)
                        .suffix(" pt"),
                );
                ui.end_row();

                ui.label("Word wrap");
                ui.checkbox(&mut self.word_wrap, "");
                ui.end_row();

                ui.label("Line numbers");
                ui.checkbox(&mut self.line_numbers, "");
                ui.end_row();

                ui.label("Terminal shell");
                let mut shell = self.terminal_shell.clone().unwrap_or_default();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut shell)
                        .hint_text(if cfg!(windows) {
                            "powershell.exe"
                        } else {
                            "$SHELL"
                        })
                        .desired_width(220.0),
                );
                if response.changed() {
                    self.terminal_shell = Some(shell).filter(|s| !s.trim().is_empty());
                }
                ui.end_row();
            });
        *self != before
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let settings = Settings {
            theme: "InspiredGitHub".into(),
            font_size: 16.0,
            word_wrap: true,
            line_numbers: false,
            terminal_shell: Some("bash --login".into()),
        };
        let text = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(Settings::parse(&text).unwrap(), settings);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let settings = Settings::parse("word_wrap = true").unwrap();
        assert!(settings.word_wrap);
        assert_eq!(settings.theme, DEFAULT_THEME);
        assert!(settings.line_numbers);
    }

    #[test]
    fn font_size_is_clamped() {
        assert_eq!(
            Settings::parse("font_size = 1000").unwrap().font_size,
            MAX_FONT_SIZE
        );
        assert_eq!(
            Settings::parse("font_size = 1").unwrap().font_size,
            MIN_FONT_SIZE
        );
    }

    #[test]
    fn rejects_malformed_toml() {
        assert!(Settings::parse("font_size = \"big\"").is_err());
    }

    #[test]
    fn load_and_save() {
        let dir = std::env::temp_dir().join(format!("forge-settings-{}", std::process::id()));
        let path = dir.join("nested").join("settings.toml");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let settings = Settings {
            font_size: 18.0,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! The Settings window and the editor preferences it controls (font size,
//! word wrap, line numbers), plus editing `settings.toml` directly.

use egui::TextStyle;

use super::ForgeApp;
use crate::settings::{self, Settings};
use crate::updater;

impl ForgeApp {
    /// Sets the editor (monospace) font size. Galleys and highlighted jobs
    /// carry the font, so changing it re-lays out the text.
    pub(super) fn apply_font_size(ctx: &egui::Context, size: f32) {
        ctx.all_styles_mut(|style| {
            if let Some(font) = style.text_styles.get_mut(&TextStyle::Monospace) {
                font.size = size;
            }
        });
    }

    /// Changes the font size by `delta` points, or back to the default for `None`.
    pub(super) fn change_font_size(&mut self, ctx: &egui::Context, delta: Option<f32>) {
        let size = match delta {
            Some(d) => settings::clamp_font_size(self.settings.font_size + d),
            None => settings::DEFAULT_FONT_SIZE,
        };
        self.set_font_size(ctx, size);
        self.status = format!("Font size: {size} pt");
    }

    fn set_font_size(&mut self, ctx: &egui::Context, size: f32) {
        let size = settings::clamp_font_size(size);
        if size != self.settings.font_size {
            self.update_settings(|s| s.font_size = size);
        }
        Self::apply_font_size(ctx, self.settings.font_size);
    }

    pub(super) fn toggle_word_wrap(&mut self) {
        let on = !self.settings.word_wrap;
        self.update_settings(|s| s.word_wrap = on);
        self.status = format!("Word wrap {}", if on { "on" } else { "off" });
    }

    pub(super) fn toggle_line_numbers(&mut self) {
        let on = !self.settings.line_numbers;
        self.update_settings(|s| s.line_numbers = on);
        self.status = format!("Line numbers {}", if on { "on" } else { "off" });
    }

    /// Re-reads the settings file (after it was saved from an editor tab) and
    /// applies it. An invalid file is reported and the current settings kept.
    pub(super) fn reload_settings(&mut self, ctx: &egui::Context) {
        let settings = match Settings::try_load() {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Settings not applied: {e:#}");
                return;
            }
        };
        self.settings = settings;
        Self::apply_font_size(ctx, self.settings.font_size);
        let wanted = self
            .settings
            .theme
            .clone()
            .unwrap_or_else(|| crate::theme::DEFAULT_THEME.to_owned());
        self.status = match self.themes.iter().position(|t| t.name == wanted) {
            Some(idx) => {
                if idx != self.theme {
                    self.activate_theme(ctx, idx);
                }
                "Settings reloaded".to_owned()
            }
            None => format!(
                "Settings reloaded. Unknown theme \"{wanted}\", keeping {}",
                self.current_theme().name
            ),
        };
    }

    /// Opens the settings file in a tab, writing it first if it doesn't exist
    /// so that every option is there to edit.
    pub(super) fn open_settings_file(&mut self) {
        let Some(path) = Settings::path() else {
            self.status = "Error: no config directory".to_owned();
            return;
        };
        if !path.exists()
            && let Err(e) = self.settings.save_to(&path)
        {
            self.status = format!("Error: {e:#}");
            return;
        }
        self.open_file(&path);
    }

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut theme = None;
        let mut font_size = self.settings.font_size;
        let mut word_wrap = self.settings.word_wrap;
        let mut line_numbers = self.settings.line_numbers;
        let mut format_on_save = self.settings.format_on_save;
        let mut auto_update = self
            .settings
            .auto_update
            .unwrap_or(updater::IS_RELEASE_BUILD);
        let mut changed = false;
        let mut edit_file = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Color theme");
                        egui::ComboBox::from_id_salt("settings_theme")
                            .selected_text(&self.current_theme().name)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for (idx, t) in self.themes.iter().enumerate() {
                                    if ui.selectable_label(idx == self.theme, &t.name).clicked() {
                                        theme = Some(idx);
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("Font size");
                        changed |= ui
                            .add(
                                egui::Slider::new(
                                    &mut font_size,
                                    settings::MIN_FONT_SIZE..=settings::MAX_FONT_SIZE,
                                )
                                .step_by(1.0)
                                .suffix(" pt"),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Word wrap");
                        changed |= ui.checkbox(&mut word_wrap, "").changed();
                        ui.end_row();

                        ui.label("Line numbers");
                        changed |= ui.checkbox(&mut line_numbers, "").changed();
                        ui.end_row();

                        ui.label("Format on save");
                        changed |= ui.checkbox(&mut format_on_save, "").changed();
                        ui.end_row();

                        ui.label("Check for updates on startup");
                        changed |= ui.checkbox(&mut auto_update, "").changed();
                        ui.end_row();
                    });
                ui.separator();
                if let Some(path) = Settings::path() {
                    ui.horizontal(|ui| {
                        edit_file = ui.small_button("Edit as TOML").clicked();
                        let shown = path.display().to_string();
                        ui.add(egui::Label::new(egui::RichText::new(&shown).weak()).truncate())
                            .on_hover_text(shown);
                    });
                }
            });
        self.settings_open = open && !ctx.input(|i| i.key_pressed(egui::Key::Escape));

        if let Some(idx) = theme {
            self.set_theme(ctx, idx);
        }
        if changed {
            let s = &self.settings;
            let auto_changed = auto_update != s.auto_update.unwrap_or(updater::IS_RELEASE_BUILD);
            if word_wrap != s.word_wrap
                || line_numbers != s.line_numbers
                || format_on_save != s.format_on_save
                || auto_changed
            {
                self.update_settings(|s| {
                    s.word_wrap = word_wrap;
                    s.line_numbers = line_numbers;
                    s.format_on_save = format_on_save;
                    if auto_changed {
                        s.auto_update = Some(auto_update);
                    }
                });
            }
            self.set_font_size(ctx, font_size);
        }
        if edit_file {
            self.settings_open = false;
            self.open_settings_file();
        }
    }
}

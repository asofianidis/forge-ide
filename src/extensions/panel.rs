//! The Extensions sidebar: installed extensions, their commands and themes,
//! and installing from a zip.

use std::collections::HashSet;
use std::path::PathBuf;

use egui::{RichText, Ui};

use super::{Extension, Extensions, qualified_id};
use crate::theme::UiColors;
use crate::ui::icons::{self, Icon};

/// Something the user asked for in the Extensions view.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelAction {
    Install,
    Reload,
    OpenFolder,
    ShowGuide,
    /// Run a command by qualified id.
    Run(String),
    ShowOutput(String),
    SetEnabled(String, bool),
    Uninstall(String),
    ShowDetails(String),
    Reveal(PathBuf),
    ApplyTheme(String),
}

#[derive(Default)]
pub struct ExtensionsPanel {
    filter: String,
    /// Extension whose Uninstall button was clicked once.
    confirm_uninstall: Option<String>,
}

/// What the panel needs to know about running commands.
pub struct RunState<'a> {
    pub running: &'a HashSet<String>,
    pub has_output: &'a HashSet<String>,
}

impl ExtensionsPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        exts: &Extensions,
        runs: &RunState<'_>,
        colors: &UiColors,
    ) -> Option<PanelAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("EXTENSIONS")
                    .small()
                    .strong()
                    .color(colors.text_muted.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new("⋯").strong(), |ui| {
                    if ui.button("Install from ZIP…").clicked() {
                        action = Some(PanelAction::Install);
                    }
                    if ui.button("Reload Extensions").clicked() {
                        action = Some(PanelAction::Reload);
                    }
                    if ui.button("Open Extensions Folder").clicked() {
                        action = Some(PanelAction::OpenFolder);
                    }
                    ui.separator();
                    if ui.button("Extension Authoring Guide").clicked() {
                        action = Some(PanelAction::ShowGuide);
                    }
                });
                if icons::button(ui, Icon::Refresh, "Reload extensions").clicked() {
                    action = Some(PanelAction::Reload);
                }
                if icons::button(ui, Icon::Plus, "Install from ZIP…").clicked() {
                    action = Some(PanelAction::Install);
                }
            });
        });

        if exts.list.is_empty() {
            ui.add_space(8.0);
            ui.label("No extensions installed.");
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Extensions add commands and color themes. Install one from a .zip \
                     file, or drop a .zip onto the window.",
                )
                .color(colors.text_muted.0),
            );
            ui.add_space(6.0);
            if ui.button("Install from ZIP…").clicked() {
                action = Some(PanelAction::Install);
            }
            if ui.link("How to write an extension").clicked() {
                action = Some(PanelAction::ShowGuide);
            }
            return action;
        }

        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter extensions")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        let filter = self.filter.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for ext in &exts.list {
                    if !filter.is_empty() && !matches_filter(ext, &filter) {
                        continue;
                    }
                    if let Some(a) = self.card(ui, ext, runs, colors) {
                        action = Some(a);
                    }
                    ui.add_space(6.0);
                }
            });
        action
    }

    fn card(
        &mut self,
        ui: &mut Ui,
        ext: &Extension,
        runs: &RunState<'_>,
        colors: &UiColors,
    ) -> Option<PanelAction> {
        let mut action = None;
        let id = ext.id();
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let name = RichText::new(ext.name()).strong();
                    ui.add(
                        egui::Label::new(if ext.is_active() {
                            name
                        } else {
                            name.color(colors.text_muted.0)
                        })
                        .truncate(),
                    );
                    if let Some(m) = &ext.manifest {
                        ui.label(RichText::new(&m.version).small().color(colors.text_muted.0));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ext.manifest.is_some() {
                            let mut on = ext.enabled;
                            let tip = if on { "Disable" } else { "Enable" };
                            if ui.checkbox(&mut on, "").on_hover_text(tip).changed() {
                                action = Some(PanelAction::SetEnabled(id.clone(), on));
                            }
                        }
                    });
                });
                if let Some(m) = &ext.manifest {
                    if !m.description.is_empty() {
                        ui.label(RichText::new(&m.description).color(colors.text_muted.0));
                    }
                    if !m.author.is_empty() {
                        ui.label(
                            RichText::new(format!("by {}", m.author))
                                .small()
                                .color(colors.text_muted.0),
                        );
                    }
                }
                if let Some(e) = &ext.error {
                    ui.colored_label(colors.error.0, e);
                }
                for w in &ext.warnings {
                    ui.colored_label(colors.warning.0, w);
                }
                if ext.is_active() {
                    if let Some(a) = commands(ui, ext, &id, runs, colors) {
                        action = Some(a);
                    }
                    for name in &ext.theme_names {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("◐").color(colors.text_muted.0));
                            if ui
                                .link(format!("Theme: {name}"))
                                .on_hover_text("Use this color theme")
                                .clicked()
                            {
                                action = Some(PanelAction::ApplyTheme(name.clone()));
                            }
                        });
                    }
                }
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if ui.small_button("Details").clicked() {
                        action = Some(PanelAction::ShowDetails(id.clone()));
                    }
                    if ui
                        .small_button("Folder")
                        .on_hover_text(ext.dir.display().to_string())
                        .clicked()
                    {
                        action = Some(PanelAction::Reveal(ext.dir.clone()));
                    }
                    if self.confirm_uninstall.as_deref() == Some(id.as_str()) {
                        let confirm = egui::Button::new(
                            RichText::new("Really uninstall?").color(colors.error.0),
                        )
                        .small();
                        if ui.add(confirm).clicked() {
                            self.confirm_uninstall = None;
                            action = Some(PanelAction::Uninstall(id.clone()));
                        }
                        if ui.small_button("Cancel").clicked() {
                            self.confirm_uninstall = None;
                        }
                    } else if ui.small_button("Uninstall").clicked() {
                        self.confirm_uninstall = Some(id.clone());
                    }
                });
            });
        action
    }
}

fn commands(
    ui: &mut Ui,
    ext: &Extension,
    id: &str,
    runs: &RunState<'_>,
    colors: &UiColors,
) -> Option<PanelAction> {
    let mut action = None;
    for cmd in ext.commands() {
        let qid = qualified_id(id, &cmd.id);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if runs.running.contains(&qid) {
                ui.spinner();
            } else if icons::button_sized(ui, Icon::Play, &format!("Run {}", cmd.title), 18.0)
                .clicked()
            {
                action = Some(PanelAction::Run(qid.clone()));
            }
            let mut tip = qid.clone();
            if !cmd.file_types.is_empty() {
                tip.push_str(&format!("\nFile types: {}", cmd.file_types.join(", ")));
            }
            if cmd.on_save {
                tip.push_str("\nRuns when a matching file is saved");
            }
            let label = ui
                .add(
                    egui::Label::new(&cmd.title)
                        .sense(egui::Sense::click())
                        .truncate(),
                )
                .on_hover_text(tip);
            if label.clicked() {
                action = Some(PanelAction::Run(qid.clone()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if runs.has_output.contains(&qid)
                    && icons::button_sized(ui, Icon::OpenFile, "Show last output", 18.0).clicked()
                {
                    action = Some(PanelAction::ShowOutput(qid.clone()));
                }
                if let Some(s) = cmd.shortcut() {
                    ui.label(
                        RichText::new(ui.ctx().format_shortcut(&s))
                            .small()
                            .color(colors.text_muted.0),
                    );
                }
                if cmd.on_save {
                    ui.label(RichText::new("on save").small().color(colors.text_muted.0));
                }
            });
        });
    }
    action
}

fn matches_filter(ext: &Extension, filter: &str) -> bool {
    let mut haystack = format!("{} {}", ext.id(), ext.name());
    if let Some(m) = &ext.manifest {
        haystack.push_str(&format!(" {} {}", m.description, m.author));
        for c in &m.commands {
            haystack.push(' ');
            haystack.push_str(&c.title);
        }
    }
    haystack.to_lowercase().contains(filter)
}

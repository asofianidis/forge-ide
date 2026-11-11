//! The "Language Servers" window: search the catalog, see what is installed,
//! and install servers with one click.

use egui::{Color32, RichText};

use super::catalog::{self, ServerSpec};
use super::install::JobState;
use super::{ServerManager, ServerStatus};
use crate::workspace::FileKinds;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    /// Servers for file types present in the workspace or open tabs.
    Project,
    All,
    Installed,
}

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Filter::Project => "Used in this project",
            Filter::All => "All",
            Filter::Installed => "Installed",
        }
    }
}

pub struct ServerBrowser {
    pub open: bool,
    query: String,
    filter: Filter,
}

impl Default for ServerBrowser {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            filter: Filter::Project,
        }
    }
}

const GREEN: Color32 = Color32::from_rgb(120, 200, 120);
const YELLOW: Color32 = Color32::from_rgb(220, 190, 90);
const RED: Color32 = Color32::from_rgb(230, 110, 110);

impl ServerBrowser {
    /// Opens the window searching for `query` across all servers.
    pub fn open_with_query(&mut self, query: &str) {
        self.open = true;
        self.query = query.to_owned();
        self.filter = Filter::All;
    }

    pub fn show(&mut self, ctx: &egui::Context, manager: &mut ServerManager, project: &FileKinds) {
        let mut open = self.open;
        egui::Window::new("Language Servers")
            .open(&mut open)
            .default_size([560.0, 520.0])
            .collapsible(false)
            .show(ctx, |ui| self.contents(ui, manager, project));
        self.open = open;
    }

    fn contents(&mut self, ui: &mut egui::Ui, manager: &mut ServerManager, project: &FileKinds) {
        ui.label(
            "Language servers add completion, diagnostics and navigation for a language. \
             Install the ones you need; Forge uses each language's own package manager.",
        );
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search by language, name or extension…")
                    .desired_width(260.0),
            );
            for f in [Filter::Project, Filter::All, Filter::Installed] {
                ui.selectable_value(&mut self.filter, f, f.label());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if manager.is_detecting() {
                    ui.spinner();
                } else if ui
                    .button("⟳")
                    .on_hover_text("Re-scan installed servers")
                    .clicked()
                {
                    manager.refresh();
                }
            });
        });
        ui.separator();

        let visible: Vec<&'static ServerSpec> = catalog::servers()
            .iter()
            .filter(|s| s.matches_query(&self.query))
            .filter(|s| match self.filter {
                Filter::All => true,
                Filter::Project => project.uses(s),
                Filter::Installed => manager.is_installed(s.id),
            })
            .collect();

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if visible.is_empty() {
                    ui.add_space(8.0);
                    ui.label(match self.filter {
                        Filter::Project if self.query.is_empty() => {
                            "No known language servers match the files in this project. \
                             Try the \"All\" tab."
                        }
                        Filter::Installed if self.query.is_empty() => {
                            "No language servers detected yet."
                        }
                        _ => "No language servers match your search.",
                    });
                }
                for spec in visible {
                    server_card(ui, spec, manager, project.uses(spec));
                    ui.add_space(4.0);
                }
            });
    }
}

fn server_card(
    ui: &mut egui::Ui,
    spec: &'static ServerSpec,
    manager: &mut ServerManager,
    in_project: bool,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());

        ui.horizontal(|ui| {
            ui.label(RichText::new(spec.name).strong().size(15.0));
            ui.label(RichText::new(spec.languages.join(", ")).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match manager.status(spec.id) {
                    ServerStatus::Checking => {
                        ui.label(RichText::new("checking…").weak());
                    }
                    ServerStatus::Installed(path) => {
                        let launch = std::iter::once(path.display().to_string())
                            .chain(spec.launch_args.iter().map(|a| a.to_string()))
                            .collect::<Vec<_>>()
                            .join(" ");
                        ui.label(RichText::new("✔ Installed").color(GREEN))
                            .on_hover_text(format!(
                                "Launch command:
{launch}"
                            ));
                    }
                    ServerStatus::Missing => {
                        ui.label(RichText::new("Not installed").weak());
                    }
                }
                if in_project {
                    ui.label(RichText::new("used in project").small().color(YELLOW));
                }
            });
        });
        ui.label(spec.description);

        if let Some(job) = manager.job(spec.id) {
            job_section(ui, spec, manager, job.state(), job.log());
        } else if !manager.is_installed(spec.id) {
            install_row(ui, spec, manager);
        }

        ui.horizontal(|ui| {
            ui.hyperlink_to("Homepage ↗", spec.homepage);
        });
    });
}

fn install_row(ui: &mut egui::Ui, spec: &'static ServerSpec, manager: &mut ServerManager) {
    let Some(method) = manager.preferred_method(spec) else {
        ui.label(
            RichText::new("No automatic installer for this platform — see the homepage.")
                .color(YELLOW),
        );
        return;
    };
    let command = method.command_line();
    let available = manager.tool_path(method.program).is_some();

    ui.horizontal(|ui| {
        let install = ui
            .add_enabled(available, egui::Button::new("⬇ Install"))
            .on_hover_text(format!("Runs: {command}"))
            .on_disabled_hover_text(format!(
                "`{}` was not found on PATH. Install it first, or run the command yourself.",
                method.program
            ));
        if install.clicked() {
            manager.install(spec, method);
        }
        ui.code(&command);
        if ui
            .small_button("📋")
            .on_hover_text("Copy command")
            .clicked()
        {
            ui.ctx().copy_text(command.clone());
        }
    });
    if !available && !manager.is_detecting() {
        ui.label(
            RichText::new(format!(
                "Requires `{}`, which was not found.",
                method.program
            ))
            .small()
            .color(YELLOW),
        );
    }
}

fn job_section(
    ui: &mut egui::Ui,
    spec: &'static ServerSpec,
    manager: &mut ServerManager,
    state: JobState,
    log: String,
) {
    ui.horizontal(|ui| match &state {
        JobState::Running => {
            ui.spinner();
            ui.label("Installing…");
        }
        JobState::Succeeded => {
            ui.label(RichText::new("Install finished.").color(GREEN));
            if !manager.is_installed(spec.id) && !manager.is_detecting() {
                ui.label(
                    RichText::new(format!(
                        "`{}` still isn't on PATH; you may need to restart Forge.",
                        spec.command
                    ))
                    .small()
                    .color(YELLOW),
                );
            }
        }
        JobState::Failed(err) => {
            ui.label(RichText::new(format!("Install failed: {err}")).color(RED));
        }
    });

    egui::CollapsingHeader::new("Output")
        .id_salt(("install-log", spec.id))
        .default_open(matches!(state, JobState::Failed(_)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(log).monospace().small()).wrap());
                });
        });

    if state != JobState::Running && ui.small_button("Dismiss").clicked() {
        manager.dismiss_job(spec.id);
    }
}

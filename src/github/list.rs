//! The Pull Requests sidebar: connection state, filters, the PR list and a
//! form for opening a new pull request from the current branch.

use egui::{Color32, RichText, Sense, Ui, UiBuilder, vec2};

use super::model::{CheckOutcome, PrSummary, rollup};
use super::{Connection, GitHub, NewPr, PrFilter, PrState};
use crate::theme::UiColors;
use crate::ui::fmt;
use crate::ui::icons::{self, Icon};
use crate::ui::markdown::label_color;

/// Local git facts the panel needs (it doesn't talk to git itself).
#[derive(Debug, Clone, Default)]
pub struct BranchContext {
    pub branch: Option<String>,
    pub has_upstream: bool,
    pub remote: Option<String>,
    /// Subject of the latest commit, used to prefill the PR title.
    pub last_subject: Option<String>,
    /// Commits not yet pushed.
    pub ahead: u32,
}

pub enum ListAction {
    Open(u64),
}

#[derive(Default)]
pub struct PrListPanel {
    creating: bool,
    form: NewPr,
    /// Branch the form was prefilled for.
    form_branch: Option<String>,
}

pub fn state_color(state: &str, draft: bool, colors: &UiColors) -> Color32 {
    match (state, draft) {
        ("MERGED", _) => Color32::from_rgb(0xa3, 0x71, 0xf7),
        ("CLOSED", _) => colors.deleted.0,
        (_, true) => colors.text_muted.0,
        _ => colors.added.0,
    }
}

pub fn state_label(state: &str, draft: bool) -> &'static str {
    match (state, draft) {
        ("MERGED", _) => "Merged",
        ("CLOSED", _) => "Closed",
        (_, true) => "Draft",
        _ => "Open",
    }
}

pub fn check_glyph(
    outcome: CheckOutcome,
    colors: &UiColors,
) -> (&'static str, Color32, &'static str) {
    match outcome {
        CheckOutcome::Pass => ("✔", colors.added.0, "All checks passed"),
        CheckOutcome::Fail => ("✖", colors.deleted.0, "Some checks failed"),
        CheckOutcome::Pending => ("●", colors.warning.0, "Checks in progress"),
        CheckOutcome::Skipped => ("–", colors.text_muted.0, "Checks skipped"),
    }
}

pub fn review_badge(decision: &str, colors: &UiColors) -> Option<(&'static str, Color32)> {
    match decision {
        "APPROVED" => Some(("Approved", colors.added.0)),
        "CHANGES_REQUESTED" => Some(("Changes requested", colors.deleted.0)),
        "REVIEW_REQUIRED" => Some(("Review required", colors.warning.0)),
        _ => None,
    }
}

impl PrListPanel {
    /// Opens the "new pull request" form.
    pub fn start_create(&mut self) {
        self.creating = true;
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        gh: &mut GitHub,
        branch: &BranchContext,
        colors: &UiColors,
    ) -> Option<ListAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("PULL REQUESTS")
                    .small()
                    .strong()
                    .color(colors.text_muted.0),
            );
            if gh.is_loading() {
                ui.spinner();
            }
            if !gh.is_ready() {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icons::button(ui, Icon::Refresh, "Refresh").clicked() {
                    gh.refresh();
                }
                if icons::button(ui, Icon::Plus, "Create pull request").clicked() {
                    self.creating = !self.creating;
                }
            });
        });

        match gh.connection.clone() {
            Connection::NoFolder => {
                ui.label(RichText::new("Open a folder to see its pull requests.").weak());
                return None;
            }
            Connection::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Connecting to GitHub…");
                });
                return None;
            }
            Connection::GhMissing => {
                ui.label("The GitHub CLI (gh) is required for pull requests.");
                ui.hyperlink_to("Install the GitHub CLI", "https://cli.github.com");
                ui.add_space(4.0);
                if ui.button("Check Again").clicked() {
                    gh.connect();
                }
                return None;
            }
            Connection::LoggedOut(msg) => {
                ui.label("Sign in to GitHub with the GitHub CLI:");
                ui.horizontal(|ui| {
                    ui.code("gh auth login");
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text("gh auth login".to_owned());
                    }
                });
                ui.label(RichText::new("Run it in a terminal, then check again.").weak());
                if ui.button("Check Again").clicked() {
                    gh.connect();
                }
                ui.collapsing("Details", |ui| {
                    ui.label(RichText::new(msg).small().monospace())
                });
                return None;
            }
            Connection::NoRepo(msg) => {
                ui.label("This folder isn't connected to a GitHub repository.");
                ui.label(RichText::new(msg).small().weak());
                if ui.button("Check Again").clicked() {
                    gh.connect();
                }
                return None;
            }
            Connection::Ready => {}
        }

        ui.horizontal(|ui| {
            ui.hyperlink_to(
                RichText::new(&gh.repo.name_with_owner).small(),
                &gh.repo.url,
            );
            if !gh.user.is_empty() {
                ui.label(RichText::new(format!("· @{}", gh.user)).small().weak());
            }
        });

        if self.creating {
            self.create_form(ui, gh, branch, colors);
            ui.separator();
        }

        let mut changed = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("pr_state")
                .selected_text(gh.state.label())
                .width(80.0)
                .show_ui(ui, |ui| {
                    for s in PrState::ALL {
                        changed |= ui.selectable_value(&mut gh.state, s, s.label()).changed();
                    }
                });
            egui::ComboBox::from_id_salt("pr_filter")
                .selected_text(gh.filter.label())
                .width(ui.available_width() - 8.0)
                .show_ui(ui, |ui| {
                    for f in PrFilter::ALL {
                        changed |= ui.selectable_value(&mut gh.filter, f, f.label()).changed();
                    }
                });
        });
        let search = ui.add(
            egui::TextEdit::singleline(&mut gh.query)
                .hint_text("Search (e.g. label:bug author:octocat)")
                .desired_width(f32::INFINITY),
        );
        if search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            changed = true;
        }
        if changed {
            gh.refresh();
        }
        ui.add_space(4.0);

        if let Some(err) = &gh.list_error {
            ui.colored_label(colors.error.0, err);
        }

        let current = branch
            .branch
            .as_deref()
            .and_then(|b| gh.pr_for_branch(b))
            .map(|p| p.number);
        egui::ScrollArea::vertical()
            .id_salt("pr_list")
            .auto_shrink(false)
            .show(ui, |ui| {
                if gh.prs.is_empty() && gh.list_error.is_none() && !gh.is_loading() {
                    ui.label(RichText::new("No pull requests match.").weak());
                }
                // The current branch's PR first.
                let ordered = gh
                    .prs
                    .iter()
                    .filter(|p| Some(p.number) == current)
                    .chain(gh.prs.iter().filter(|p| Some(p.number) != current));
                for pr in ordered {
                    if pr_row(ui, pr, Some(pr.number) == current, colors) {
                        action = Some(ListAction::Open(pr.number));
                    }
                }
            });
        action
    }

    fn create_form(
        &mut self,
        ui: &mut Ui,
        gh: &mut GitHub,
        branch: &BranchContext,
        colors: &UiColors,
    ) {
        let Some(head) = branch.branch.clone() else {
            ui.label(RichText::new("Check out a branch to create a pull request.").weak());
            return;
        };
        if self.form_branch.as_deref() != Some(head.as_str()) {
            self.form = NewPr {
                title: branch.last_subject.clone().unwrap_or_default(),
                base: gh.repo.default_branch_ref.name.clone(),
                ..NewPr::default()
            };
            self.form_branch = Some(head.clone());
        }
        ui.label(RichText::new("New Pull Request").strong());
        if let Some(existing) = gh.pr_for_branch(&head) {
            ui.colored_label(
                colors.warning.0,
                format!("#{} is already open for {head}.", existing.number),
            );
        }
        egui::Grid::new("new_pr")
            .num_columns(2)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                ui.label("From");
                ui.label(RichText::new(&head).monospace());
                ui.end_row();
                ui.label("Into");
                ui.add(
                    egui::TextEdit::singleline(&mut self.form.base).desired_width(f32::INFINITY),
                );
                ui.end_row();
            });
        ui.add(
            egui::TextEdit::singleline(&mut self.form.title)
                .hint_text("Title (empty: fill from commits)")
                .desired_width(f32::INFINITY),
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.form.body)
                .hint_text("Description (Markdown)")
                .desired_rows(5)
                .desired_width(f32::INFINITY),
        );
        ui.checkbox(&mut self.form.draft, "Create as draft");
        if !branch.has_upstream {
            ui.label(
                RichText::new(format!(
                    "{head} isn't on the remote yet; it will be pushed to {}.",
                    branch.remote.as_deref().unwrap_or("the remote")
                ))
                .small()
                .weak(),
            );
        } else if branch.ahead > 0 {
            ui.label(
                RichText::new(format!(
                    "{} local commit(s) aren't pushed yet; push first to include them.",
                    branch.ahead
                ))
                .small()
                .color(colors.warning.0),
            );
        }
        ui.horizontal(|ui| {
            let busy = gh.is_creating();
            let can = !busy && (branch.has_upstream || branch.remote.is_some());
            let button = egui::Button::new(RichText::new("Create").color(colors.accent_text.0))
                .fill(colors.accent.0);
            if ui.add_enabled(can, button).clicked() {
                let mut new = self.form.clone();
                new.head = head.clone();
                new.push_to = (!branch.has_upstream)
                    .then(|| branch.remote.clone())
                    .flatten();
                gh.create(new);
                self.creating = false;
                self.form_branch = None;
            }
            if busy {
                ui.spinner();
            }
            if ui.button("Cancel").clicked() {
                self.creating = false;
            }
        });
    }
}

/// One PR in the list; returns true when clicked.
fn pr_row(ui: &mut Ui, pr: &PrSummary, is_current: bool, colors: &UiColors) -> bool {
    let has_labels = !pr.labels.is_empty();
    let height = if has_labels { 84.0 } else { 64.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response.clicked();
    }
    let hovered = response.hovered();
    if hovered || is_current {
        let fill = if is_current {
            colors.accent.0.gamma_multiply(0.14)
        } else {
            ui.visuals()
                .widgets
                .hovered
                .weak_bg_fill
                .gamma_multiply(0.5)
        };
        ui.painter().rect_filled(rect, 4, fill);
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 3.0))));
    let ui = &mut child;
    ui.spacing_mut().item_spacing = vec2(4.0, 1.0);
    ui.horizontal(|ui| {
        icons::label(
            ui,
            Icon::PullRequest,
            state_color(&pr.state, pr.is_draft, colors),
            16.0,
        )
        .on_hover_text(state_label(&pr.state, pr.is_draft));
        ui.add(
            egui::Label::new(RichText::new(format!("#{} {}", pr.number, pr.title)).strong())
                .truncate()
                .selectable(false),
        );
    });
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        let mut meta = format!(
            "{} · {} · {}",
            pr.author.display(),
            pr.head_ref_name,
            fmt::relative_iso(&pr.updated_at)
        );
        if is_current {
            meta = format!("current branch · {meta}");
        }
        ui.add(
            egui::Label::new(RichText::new(meta).small().color(colors.text_muted.0))
                .truncate()
                .selectable(false),
        );
    });
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        if let Some(outcome) = rollup(&pr.status_check_rollup) {
            let (glyph, color, tip) = check_glyph(outcome, colors);
            ui.label(RichText::new(glyph).small().color(color))
                .on_hover_text(tip);
        }
        if let Some((text, color)) = review_badge(&pr.review_decision, colors) {
            ui.label(RichText::new(text).small().color(color));
        }
        ui.label(
            RichText::new(format!("+{}", pr.additions))
                .small()
                .color(colors.added.0),
        );
        ui.label(
            RichText::new(format!("−{}", pr.deletions))
                .small()
                .color(colors.deleted.0),
        );
    });
    if has_labels {
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            for l in pr.labels.iter().take(4) {
                label_chip(ui, &l.name, &l.color);
            }
        });
    }
    response
        .on_hover_text(format!(
            "{} → {}\n{}",
            pr.head_ref_name, pr.base_ref_name, pr.url
        ))
        .clicked()
}

pub fn label_chip(ui: &mut Ui, name: &str, hex: &str) {
    let color = label_color(hex);
    let text_color = if color.r() as u32 + color.g() as u32 + color.b() as u32 > 420 {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    egui::Frame::new()
        .fill(color)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(6, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(name).small().color(text_color))
        });
}

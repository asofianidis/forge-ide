//! The BYO AI sidebar (threads and one-click git) and the thread tab.

use egui::{Key, Modifiers, RichText, Sense, Ui, vec2};

use super::flow::FlowKind;
use super::picker::{self, Catalog};
use super::thread::Message;
use super::{Ai, Provider};
use crate::settings::AiSettings;
use crate::theme::UiColors;
use crate::ui::icons::{self, Icon};

/// Repository state the git card needs.
#[derive(Debug, Clone, Default)]
pub struct GitSummary {
    pub is_repo: bool,
    pub branch: Option<String>,
    pub changes: usize,
    pub staged: usize,
    pub conflicts: usize,
    pub ahead: usize,
    pub has_upstream: bool,
    pub has_remote: bool,
    pub gh_ready: bool,
    pub default_branch: String,
    /// The open PR for this branch: number and title.
    pub pr: Option<(u64, String)>,
    /// A git operation is already running.
    pub busy: bool,
}

impl GitSummary {
    fn on_default_branch(&self) -> bool {
        !self.default_branch.is_empty() && self.branch.as_deref() == Some(&self.default_branch)
    }

    fn can_open_pr(&self) -> bool {
        self.gh_ready && self.pr.is_none() && self.branch.is_some() && !self.on_default_branch()
    }

    fn needs_push(&self) -> bool {
        self.has_remote && self.branch.is_some() && (self.ahead > 0 || !self.has_upstream)
    }

    /// Every flow that makes sense right now, most complete first.
    pub fn flows(&self) -> Vec<FlowKind> {
        let mut out = Vec::new();
        if !self.is_repo || self.conflicts > 0 {
            return out;
        }
        if self.changes > 0 {
            if self.has_remote && self.branch.is_some() && self.can_open_pr() {
                out.push(FlowKind::CommitPushPr);
            }
            if self.has_remote && self.branch.is_some() {
                out.push(FlowKind::CommitPush);
            }
            out.push(FlowKind::Commit);
        } else if self.needs_push() {
            if self.can_open_pr() {
                out.push(FlowKind::PushPr);
            }
            out.push(FlowKind::Push);
        } else if self.can_open_pr() && self.has_upstream {
            out.push(FlowKind::CreatePr);
        }
        out
    }
}

pub enum SidebarAction {
    OpenThread(String),
    /// Close the tab of a deleted thread.
    ThreadDeleted(String),
    StartFlow(FlowKind),
    /// Open the Settings window (where the default models are chosen).
    OpenSettings,
    OpenPr(u64),
}

pub enum ThreadAction {
    SettingsChanged,
}

fn section(ui: &mut Ui, title: &str, colors: &UiColors) {
    ui.add_space(6.0);
    ui.label(
        RichText::new(title)
            .small()
            .strong()
            .color(colors.text_muted.0),
    );
    ui.add_space(2.0);
}

impl Ai {
    /// The sidebar. Returns what the app should do.
    pub fn sidebar(
        &mut self,
        ui: &mut Ui,
        settings: &mut AiSettings,
        git: &GitSummary,
        colors: &UiColors,
    ) -> Vec<SidebarAction> {
        let mut actions = Vec::new();
        ui.horizontal(|ui| {
            ui.label(RichText::new("BYO AI").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Refresh, "Look for installed CLIs again").clicked() {
                    self.redetect();
                }
                let can_create = self.folder.is_some();
                let plus =
                    ui.add_enabled_ui(can_create, |ui| icons::button(ui, Icon::Plus, "New thread"));
                if plus.inner.clicked()
                    && let Some(id) = self.new_thread(settings)
                {
                    actions.push(SidebarAction::OpenThread(id));
                }
            });
        });

        if self.folder.is_none() {
            ui.add_space(8.0);
            ui.label(RichText::new("Open a folder to start a thread.").color(colors.text_muted.0));
            return actions;
        }
        if self.first_installed().is_none() {
            ui.add_space(6.0);
            ui.label(
                RichText::new("No agent CLI found. Install one, then press ⟳:")
                    .color(colors.warning.0),
            );
            for p in Provider::ALL {
                ui.label(RichText::new(format!("{}: {}", p.name(), p.install_hint())).small());
            }
        }

        section(ui, "THREADS", colors);
        let list: Vec<(String, String, String, u64, bool)> = self
            .folder_threads()
            .into_iter()
            .map(|t| {
                (
                    t.id.clone(),
                    t.title.clone(),
                    format!("{} · {}", t.choice.label(), t.choice.provider.name()),
                    t.updated,
                    self.is_running(&t.id),
                )
            })
            .collect();
        let list_height = (ui.available_height() - 210.0).max(120.0);
        egui::ScrollArea::vertical()
            .id_salt("ai_threads")
            .max_height(list_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if list.is_empty() {
                    ui.label(
                        RichText::new("No threads in this folder yet. Press + to start one.")
                            .small()
                            .color(colors.text_muted.0),
                    );
                }
                for (id, title, model, updated, running) in &list {
                    if let Some((rid, name)) = &mut self.renaming
                        && rid == id
                    {
                        let r =
                            ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                        r.request_focus();
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
                        if enter || r.lost_focus() {
                            let (rid, name) = self.renaming.take().unwrap();
                            if enter && !name.trim().is_empty() {
                                if let Some(t) = self.thread_mut(&rid) {
                                    t.title = name.trim().to_owned();
                                    t.title_locked = true;
                                }
                                self.dirty = true;
                                self.save();
                            }
                        }
                        continue;
                    }
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    if response.hovered() {
                        ui.painter().rect_filled(rect, 6.0, colors.widget.0);
                    }
                    let left = rect.left() + 8.0;
                    let mut title_right = rect.right() - 8.0;
                    let when = crate::ui::fmt::relative_time(*updated as i64);
                    let time = ui.painter().layout_no_wrap(
                        when,
                        egui::FontId::proportional(11.0),
                        colors.text_muted.0,
                    );
                    title_right -= time.size().x;
                    ui.painter().galley(
                        egui::pos2(title_right, rect.top() + 5.0),
                        time,
                        colors.text_muted.0,
                    );
                    if *running {
                        let s = egui::Rect::from_center_size(
                            egui::pos2(title_right - 10.0, rect.top() + 12.0),
                            vec2(12.0, 12.0),
                        );
                        egui::Spinner::new().size(12.0).paint_at(ui, s);
                        title_right -= 20.0;
                    }
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        title.clone(),
                        font,
                        colors.text.0,
                    );
                    job.wrap =
                        egui::text::TextWrapping::truncate_at_width(title_right - left - 6.0);
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    ui.painter()
                        .galley(egui::pos2(left, rect.top() + 3.0), g, colors.text.0);
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        model.clone(),
                        egui::FontId::proportional(11.0),
                        colors.text_muted.0,
                    );
                    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 16.0);
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    ui.painter().galley(
                        egui::pos2(left, rect.top() + 22.0),
                        g,
                        colors.text_muted.0,
                    );
                    if response.clicked() {
                        actions.push(SidebarAction::OpenThread(id.clone()));
                    }
                    response.context_menu(|ui| {
                        if ui.button("Open").clicked() {
                            actions.push(SidebarAction::OpenThread(id.clone()));
                        }
                        if ui.button("Rename…").clicked() {
                            self.renaming = Some((id.clone(), title.clone()));
                        }
                        if *running && ui.button("Stop").clicked() {
                            self.stop(id);
                        }
                        ui.separator();
                        if ui.button("Delete").clicked() {
                            self.delete_thread(id);
                            actions.push(SidebarAction::ThreadDeleted(id.clone()));
                        }
                    });
                }
            });

        ui.add_space(4.0);
        ui.separator();
        self.git_card(ui, settings, git, colors, &mut actions);

        ui.add_space(4.0);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for p in Provider::ALL {
                let (text, color, tip) = match self.exe(p) {
                    Some(path) => ("✓", colors.added.0, path.display().to_string()),
                    None => (
                        "✗",
                        colors.text_muted.0,
                        format!("Not installed. {}", p.install_hint()),
                    ),
                };
                ui.label(
                    RichText::new(format!("{text} {}", p.name()))
                        .small()
                        .color(color),
                )
                .on_hover_text(tip);
            }
        });
        actions
    }

    fn git_card(
        &mut self,
        ui: &mut Ui,
        settings: &mut AiSettings,
        git: &GitSummary,
        colors: &UiColors,
        actions: &mut Vec<SidebarAction>,
    ) {
        section(ui, "GIT", colors);
        if !git.is_repo {
            ui.label(
                RichText::new("This folder isn't a git repository.")
                    .small()
                    .color(colors.text_muted.0),
            );
            return;
        }
        let summary = match (&git.branch, git.changes) {
            (Some(b), 0) => format!("{b} · no changes"),
            (Some(b), n) => format!("{b} · {n} changed file{}", if n == 1 { "" } else { "s" }),
            (None, n) => format!("detached HEAD · {n} changed"),
        };
        ui.label(RichText::new(summary).small().color(colors.text_muted.0));

        if let Some(flow) = &self.flow {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(flow.progress()).small())
                    .on_hover_text(flow.kind.label());
            });
        } else {
            let flows = git.flows();
            let busy = git.busy;
            ui.horizontal(|ui| match flows.first() {
                Some(primary) => {
                    let accent = egui::Button::new(
                        RichText::new(primary.label()).color(colors.accent_text.0),
                    )
                    .fill(colors.accent.0);
                    if ui
                        .add_enabled(!busy, accent)
                        .on_hover_text(flow_tip(*primary, git))
                        .on_disabled_hover_text("Waiting for another git operation")
                        .clicked()
                    {
                        actions.push(SidebarAction::StartFlow(*primary));
                    }
                    if flows.len() > 1 {
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.menu_button("▾", |ui| {
                                for f in &flows {
                                    if ui
                                        .button(f.label())
                                        .on_hover_text(flow_tip(*f, git))
                                        .clicked()
                                    {
                                        actions.push(SidebarAction::StartFlow(*f));
                                    }
                                }
                            });
                        });
                    }
                }
                None => match &git.pr {
                    Some((n, title)) => {
                        if ui
                            .button(format!("View PR #{n}"))
                            .on_hover_text(title)
                            .clicked()
                        {
                            actions.push(SidebarAction::OpenPr(*n));
                        }
                    }
                    None => {
                        let why = if git.conflicts > 0 {
                            "Resolve merge conflicts first"
                        } else if git.on_default_branch() && git.gh_ready {
                            "Nothing to commit. Switch to a feature branch to open a PR."
                        } else {
                            "Nothing to commit"
                        };
                        ui.label(RichText::new(why).small().color(colors.text_muted.0));
                    }
                },
            });
        }
        if let Some((ok, msg, url)) = &self.flow_result {
            let color = if *ok { colors.added.0 } else { colors.error.0 };
            ui.label(RichText::new(msg).small().color(color));
            if let Some(url) = url {
                ui.hyperlink_to(RichText::new(url).small(), url);
            }
        }

        ui.add_space(2.0);
        let text = self.text_choice(settings);
        let note = if self.is_installed(text.provider) {
            format!("Text by {} · change in Settings", text.label())
        } else {
            format!(
                "Built-in text ({} CLI missing) · change in Settings",
                text.provider.name()
            )
        };
        if ui
            .add(
                egui::Label::new(RichText::new(note).small().color(colors.text_muted.0))
                    .sense(Sense::click()),
            )
            .on_hover_text(
                "Open Settings to pick the model that writes commit messages and PR text",
            )
            .clicked()
        {
            actions.push(SidebarAction::OpenSettings);
        }
    }

    /// Rows for the Settings window's grid: the default agent model (and its
    /// access) and the text model. Returns whether anything changed.
    pub fn settings_rows(
        &mut self,
        ui: &mut Ui,
        settings: &mut AiSettings,
        colors: &UiColors,
    ) -> bool {
        let installed: Vec<Provider> = Provider::ALL
            .into_iter()
            .filter(|p| self.is_installed(*p))
            .collect();
        let is_installed = |p: Provider| installed.contains(&p);
        let mut changed = false;

        ui.label("Default AI model")
            .on_hover_text("Model and access new BYO AI threads start with");
        let mut agent = self.agent_choice(settings);
        let mut access = settings.access;
        ui.horizontal(|ui| {
            if picker::button(
                ui,
                "settings_agent",
                &mut agent,
                Some(&mut access),
                &mut self.agent_picker,
                Catalog {
                    installed: &is_installed,
                    favorites: &mut settings.favorites,
                    custom: &mut settings.custom_models,
                },
                colors,
            ) {
                settings.agent = Some(agent);
                settings.access = access;
                changed = true;
            }
            ui.label(
                RichText::new(access.label())
                    .small()
                    .color(colors.text_muted.0),
            )
            .on_hover_text(access.describe());
        });
        ui.end_row();

        ui.label("Commit & PR text model").on_hover_text(
            "Writes commit messages and pull request text (BYO AI git button and Source Control's Generate)",
        );
        let mut text = self.text_choice(settings);
        if picker::button(
            ui,
            "settings_text",
            &mut text,
            None,
            &mut self.text_picker,
            Catalog {
                installed: &is_installed,
                favorites: &mut settings.favorites,
                custom: &mut settings.custom_models,
            },
            colors,
        ) {
            settings.text = Some(text);
            changed = true;
        }
        ui.end_row();
        changed
    }

    /// The thread tab: transcript and composer.
    pub fn thread_view(
        &mut self,
        ui: &mut Ui,
        id: &str,
        settings: &mut AiSettings,
        colors: &UiColors,
    ) -> Option<ThreadAction> {
        let Some(thread) = self.thread(id) else {
            ui.label("This thread was deleted.");
            return None;
        };
        let title = thread.title.clone();
        let folder = thread.folder.display().to_string();
        let running = self.is_running(id);
        let mut action = None;

        ui.horizontal(|ui| {
            ui.label(RichText::new(&title).strong().size(16.0));
            ui.label(RichText::new(folder).small().color(colors.text_muted.0));
        });
        ui.separator();

        // Leave room for the composer.
        let composer_height = 120.0;
        let transcript_height = (ui.available_height() - composer_height).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt(("ai_transcript", id))
            .max_height(transcript_height)
            .min_scrolled_height(transcript_height)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(860.0));
                let thread = self.thread(id).unwrap();
                if thread.messages.is_empty() && !running {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Ask {} to work in this folder.",
                                thread.choice.provider.name()
                            ))
                            .color(colors.text_muted.0),
                        );
                        ui.label(
                            RichText::new(format!(
                                "Using {}. Pick another model or access level with the button under the message box.",
                                thread.choice.label()
                            ))
                            .small()
                            .color(colors.text_muted.0),
                        );
                    });
                }
                for m in &thread.messages {
                    message(ui, m, running, colors);
                }
                if let Some(active) = self.runs.get(id) {
                    if !active.streaming.is_empty() {
                        crate::ui::markdown::show(ui, &active.streaming);
                    }
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let secs = active.run.started.elapsed().as_secs();
                        ui.label(
                            RichText::new(format!("Working… {secs}s"))
                                .small()
                                .color(colors.text_muted.0),
                        );
                    });
                }
            });

        // Composer.
        let draft_id = ui.make_persistent_id(("ai_composer", id));
        let focused = ui.memory(|m| m.has_focus(draft_id));
        let submit = focused
            && ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::COMMAND, Key::Enter)
            });
        let mut send = None;
        egui::Frame::new()
            .fill(colors.surface.0)
            .stroke(egui::Stroke::new(1.0, colors.border.0))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                let draft = self.drafts.entry(id.to_owned()).or_default();
                let provider = self.threads.iter().find(|t| t.id == id).unwrap().choice.provider;
                let edit = ui.add(
                    egui::TextEdit::multiline(draft)
                        .id(draft_id)
                        .hint_text(format!(
                            "Ask {} anything… (Enter to send, Shift+Enter for a new line)",
                            provider.name()
                        ))
                        .frame(egui::Frame::NONE)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if self.threads.iter().find(|t| t.id == id).is_some_and(|t| t.messages.is_empty())
                    && !focused
                    && !edit.has_focus()
                    && ui.memory(|m| m.focused().is_none())
                {
                    edit.request_focus();
                }
                ui.horizontal(|ui| {
                    // Model and access for this thread.
                    let installed: Vec<Provider> = Provider::ALL
                        .into_iter()
                        .filter(|p| self.is_installed(*p))
                        .collect();
                    let is_installed = |p: Provider| installed.contains(&p);
                    let t = self.threads.iter_mut().find(|t| t.id == id).unwrap();
                    let before = t.choice.provider;
                    let mut choice = t.choice.clone();
                    let mut access = t.access;
                    if picker::button(
                        ui,
                        "thread",
                        &mut choice,
                        Some(&mut access),
                        &mut self.thread_picker,
                        Catalog {
                            installed: &is_installed,
                            favorites: &mut settings.favorites,
                            custom: &mut settings.custom_models,
                        },
                        colors,
                    ) {
                        if choice.provider != before && t.session.is_some() {
                            t.session = None;
                            t.messages.push(Message::Info {
                                text: format!(
                                    "Switched to {}. It starts a fresh conversation and can't see the messages above.",
                                    choice.provider.name()
                                ),
                            });
                        }
                        // Only this thread; the default for new threads is in Settings.
                        t.choice = choice;
                        t.access = access;
                        self.dirty = true;
                        action = Some(ThreadAction::SettingsChanged);
                    }
                    ui.label(
                        RichText::new(access.label())
                            .small()
                            .color(colors.text_muted.0),
                    )
                    .on_hover_text(access.describe());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if running {
                            if icons::button_sized(ui, Icon::Stop, "Stop", 22.0).clicked() {
                                send = Some(None);
                            }
                        } else {
                            let draft = self.drafts.get(id).cloned().unwrap_or_default();
                            let can_send = !draft.trim().is_empty();
                            let r = ui.add_enabled_ui(can_send, |ui| {
                                icons::button_sized(ui, Icon::ArrowUp, "Send (Enter)", 22.0)
                            });
                            if (r.inner.clicked() || submit) && can_send {
                                send = Some(Some(draft));
                            }
                        }
                    });
                });
            });
        match send {
            Some(Some(text)) => {
                self.drafts.remove(id);
                let titler = self.text_generator(settings);
                if let Err(e) = self.send(id, text.trim().to_owned(), titler)
                    && let Some(t) = self.thread_mut(id)
                {
                    t.messages.push(Message::Error { text: e });
                }
            }
            Some(None) => self.stop(id),
            None => {}
        }
        if self.dirty && !running {
            self.save();
        }
        action
    }
}

fn flow_tip(kind: FlowKind, git: &GitSummary) -> String {
    let what = if git.staged > 0 {
        "the staged changes"
    } else {
        "all changes"
    };
    match kind {
        FlowKind::Commit => format!("Commit {what} with a generated message"),
        FlowKind::CommitPush => format!("Commit {what} with a generated message, then push"),
        FlowKind::CommitPushPr => format!(
            "Commit {what}, push, and open a pull request into {} with generated text",
            git.default_branch
        ),
        FlowKind::Push => "Push this branch".to_owned(),
        FlowKind::PushPr => format!(
            "Push, then open a pull request into {} with generated text",
            git.default_branch
        ),
        FlowKind::CreatePr => format!(
            "Open a pull request into {} with generated text",
            git.default_branch
        ),
    }
}

fn message(ui: &mut Ui, m: &Message, running: bool, colors: &UiColors) {
    match m {
        Message::User { text } => {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(colors.widget.0)
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(text);
                });
            ui.add_space(4.0);
        }
        Message::Assistant { text } => {
            crate::ui::markdown::show(ui, text);
            ui.add_space(4.0);
        }
        Message::Tool {
            id,
            name,
            detail,
            output,
            is_error,
        } => {
            let (glyph, color) = match (output, is_error) {
                (None, _) if running => ("…", colors.text_muted.0),
                (None, _) => ("•", colors.text_muted.0),
                (Some(_), true) => ("✗", colors.error.0),
                (Some(_), false) => ("✓", colors.added.0),
            };
            let header = RichText::new(format!("{glyph} {name}  {detail}"))
                .small()
                .monospace()
                .color(if *is_error {
                    colors.error.0
                } else {
                    colors.text_muted.0
                });
            match output.as_deref().filter(|o| !o.trim().is_empty()) {
                Some(out) => {
                    egui::CollapsingHeader::new(header)
                        .id_salt(("tool", id))
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(RichText::new(out).small().monospace());
                        });
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.label(header).on_hover_text(name);
                    });
                    let _ = color;
                }
            }
        }
        Message::Error { text } => {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, colors.error.0))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(text).color(colors.error.0));
                });
        }
        Message::Info { text } => {
            ui.label(RichText::new(text).small().color(colors.text_muted.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> GitSummary {
        GitSummary {
            is_repo: true,
            branch: Some("feature".into()),
            has_remote: true,
            has_upstream: true,
            gh_ready: true,
            default_branch: "main".into(),
            ..Default::default()
        }
    }

    #[test]
    fn flows_follow_repository_state() {
        let mut g = repo();
        g.changes = 2;
        assert_eq!(
            g.flows(),
            vec![
                FlowKind::CommitPushPr,
                FlowKind::CommitPush,
                FlowKind::Commit
            ]
        );

        g.changes = 0;
        g.ahead = 1;
        assert_eq!(g.flows(), vec![FlowKind::PushPr, FlowKind::Push]);

        g.ahead = 0;
        assert_eq!(g.flows(), vec![FlowKind::CreatePr]);

        g.pr = Some((3, "x".into()));
        assert!(g.flows().is_empty());
    }

    #[test]
    fn no_pr_from_the_default_branch() {
        let mut g = repo();
        g.branch = Some("main".into());
        g.changes = 1;
        assert_eq!(g.flows(), vec![FlowKind::CommitPush, FlowKind::Commit]);
    }

    #[test]
    fn conflicts_block_everything() {
        let mut g = repo();
        g.changes = 1;
        g.conflicts = 1;
        assert!(g.flows().is_empty());
    }
}

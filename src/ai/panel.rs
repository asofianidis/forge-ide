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

/// A sidebar row: id, title, model line, last activity, running, worktree
/// folder, archived.
type ThreadItem = (
    String,
    String,
    String,
    u64,
    bool,
    Option<std::path::PathBuf>,
    bool,
);

pub enum SidebarAction {
    /// Open a folder (a thread's worktree) in another Forge window.
    OpenFolderInNewWindow(std::path::PathBuf),
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
        colors: &UiColors,
    ) -> Vec<SidebarAction> {
        let mut actions = Vec::new();
        ui.horizontal(|ui| {
            ui.label(RichText::new("BYO AI").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

        let list: Vec<ThreadItem> = self
            .folder_threads()
            .into_iter()
            .map(|t| {
                (
                    t.id.clone(),
                    t.title.clone(),
                    match &t.worktree {
                        Some(wt) => format!("⎇ {} · {}", wt.branch, t.choice.label()),
                        None => format!("{} · {}", t.choice.label(), t.choice.provider.name()),
                    },
                    t.updated,
                    self.is_running(&t.id),
                    t.worktree.as_ref().map(|w| w.path.clone()),
                    t.archived,
                )
            })
            .collect();
        // Running threads first, then finished ones, then the archive.
        let working: Vec<ThreadItem> = list.iter().filter(|t| t.4).cloned().collect();
        let done: Vec<ThreadItem> = list.iter().filter(|t| !t.4 && !t.6).cloned().collect();
        let archived: Vec<ThreadItem> = list.iter().filter(|t| !t.4 && t.6).cloned().collect();
        egui::ScrollArea::vertical()
            .id_salt("ai_threads")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if list.is_empty() {
                    section(ui, "THREADS", colors);
                    ui.label(
                        RichText::new("No threads in this folder yet. Press + to start one.")
                            .small()
                            .color(colors.text_muted.0),
                    );
                }
                if !working.is_empty() {
                    section(ui, &format!("WORKING · {}", working.len()), colors);
                    self.thread_rows(ui, &working, colors, &mut actions);
                }
                if !done.is_empty() {
                    section(ui, &format!("DONE · {}", done.len()), colors);
                    self.thread_rows(ui, &done, colors, &mut actions);
                }
                if !archived.is_empty() {
                    ui.add_space(6.0);
                    egui::CollapsingHeader::new(
                        RichText::new(format!("ARCHIVED · {}", archived.len()))
                            .small()
                            .strong()
                            .color(colors.text_muted.0),
                    )
                    .id_salt("ai_archived")
                    .default_open(false)
                    .show(ui, |ui| {
                        self.thread_rows(ui, &archived, colors, &mut actions);
                    });
                }
            });
        actions
    }

    /// Thread rows in the sidebar: title, time, model; right-click for more.
    fn thread_rows(
        &mut self,
        ui: &mut Ui,
        list: &[ThreadItem],
        colors: &UiColors,
        actions: &mut Vec<SidebarAction>,
    ) {
        for (id, title, model, updated, running, wt_path, archived) in list {
            if let Some((rid, name)) = &mut self.renaming
                && rid == id
            {
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
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
            let mut job =
                egui::text::LayoutJob::simple_singleline(title.clone(), font, colors.text.0);
            job.wrap = egui::text::TextWrapping::truncate_at_width(title_right - left - 6.0);
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
            ui.painter()
                .galley(egui::pos2(left, rect.top() + 22.0), g, colors.text_muted.0);
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
                let archive = if *archived { "Unarchive" } else { "Archive" };
                if ui.button(archive).clicked() {
                    self.set_archived(id, !*archived);
                }
                if *running && ui.button("Stop").clicked() {
                    self.stop(id);
                }
                if let Some(path) = wt_path {
                    if ui.button("Open worktree in new window").clicked() {
                        actions.push(SidebarAction::OpenFolderInNewWindow(path.clone()));
                    }
                    if ui.button("Copy worktree path").clicked() {
                        ui.ctx().copy_text(path.display().to_string());
                    }
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    self.delete_thread(id);
                    actions.push(SidebarAction::ThreadDeleted(id.clone()));
                }
                if wt_path.is_some()
                    && ui
                        .button("Delete thread and worktree")
                        .on_hover_text("Also removes the worktree folder. Its branch is kept.")
                        .clicked()
                {
                    self.delete_thread_and_worktree(id);
                    actions.push(SidebarAction::ThreadDeleted(id.clone()));
                }
            });
        }
    }

    /// The one-click git split button for the tab bar, laid out right to
    /// left (the caller gives a right-to-left `ui`).
    pub fn git_button(
        &mut self,
        ui: &mut Ui,
        settings: &AiSettings,
        git: &GitSummary,
        colors: &UiColors,
    ) -> Vec<SidebarAction> {
        let mut actions = Vec::new();
        if !git.is_repo {
            return actions;
        }
        let text = self.text_choice(settings);
        let writer = if self.is_installed(text.provider) {
            format!("Text by {} (change in Settings)", text.label())
        } else {
            format!(
                "Built-in text: the {} CLI isn't installed (change in Settings)",
                text.provider.name()
            )
        };

        if let Some(flow) = &self.flow {
            ui.label(RichText::new(flow.progress()).small())
                .on_hover_text(flow.kind.label());
            ui.spinner();
            return actions;
        }

        let flows = git.flows();
        let busy = git.busy;
        ui.spacing_mut().item_spacing.x = 1.0;
        match flows.first() {
            Some(primary) => {
                if flows.len() > 1 {
                    ui.add_enabled_ui(!busy, |ui| {
                        let r =
                            ui.menu_button(RichText::new("▾").color(colors.accent_text.0), |ui| {
                                for f in &flows {
                                    if ui
                                        .button(f.label())
                                        .on_hover_text(flow_tip(*f, git))
                                        .clicked()
                                    {
                                        actions.push(SidebarAction::StartFlow(*f));
                                    }
                                }
                                ui.separator();
                                if ui.button("Change text model…").clicked() {
                                    actions.push(SidebarAction::OpenSettings);
                                }
                            });
                        r.response.on_hover_text("More git actions");
                    });
                }
                let button =
                    egui::Button::new(RichText::new(primary.label()).color(colors.accent_text.0))
                        .fill(colors.accent.0);
                if ui
                    .add_enabled(!busy, button)
                    .on_hover_text(format!("{}\n{writer}", flow_tip(*primary, git)))
                    .on_disabled_hover_text("Waiting for another git operation")
                    .clicked()
                {
                    actions.push(SidebarAction::StartFlow(*primary));
                }
            }
            None => {
                if let Some((n, title)) = &git.pr
                    && ui
                        .button(format!("View PR #{n}"))
                        .on_hover_text(title)
                        .clicked()
                {
                    actions.push(SidebarAction::OpenPr(*n));
                }
            }
        }
        ui.spacing_mut().item_spacing.x = 8.0;

        // The last run's outcome, for a little while.
        if let Some((ok, msg, url, at)) = &self.flow_result
            && at.elapsed() < std::time::Duration::from_secs(30)
        {
            if *ok {
                match url {
                    Some(url) => {
                        ui.hyperlink_to(
                            RichText::new("✓ PR opened ↗").small().color(colors.added.0),
                            url,
                        )
                        .on_hover_text(msg);
                    }
                    None => {
                        ui.label(RichText::new("✓ Done").small().color(colors.added.0))
                            .on_hover_text(msg);
                    }
                }
            } else {
                ui.label(RichText::new("✗ Failed").small().color(colors.error.0))
                    .on_hover_text(msg);
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        actions
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

        ui.label("Agent CLIs")
            .on_hover_text("The command-line agents BYO AI found on this machine");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for p in Provider::ALL {
                match self.exe(p) {
                    Some(path) => {
                        ui.label(
                            RichText::new(format!("✓ {}", p.name()))
                                .small()
                                .color(colors.added.0),
                        )
                        .on_hover_text(path.display().to_string());
                    }
                    None => {
                        let r = ui
                            .add(
                                egui::Label::new(
                                    RichText::new(format!("✗ {}", p.name()))
                                        .small()
                                        .color(colors.text_muted.0),
                                )
                                .sense(Sense::click()),
                            )
                            .on_hover_text(format!(
                                "Not installed. Click to copy the install command:\n{}",
                                p.install_hint()
                            ));
                        if r.clicked() {
                            ui.ctx().copy_text(p.install_hint().to_owned());
                        }
                    }
                }
            }
            if icons::button(ui, Icon::Refresh, "Look for installed CLIs again").clicked() {
                self.redetect();
            }
        });
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
        let branch = thread
            .worktree
            .as_ref()
            .map(|w| (w.branch.clone(), w.base.clone()));
        let running = self.is_running(id);
        let mut action = None;

        ui.horizontal(|ui| {
            ui.label(RichText::new(&title).strong().size(16.0));
            if let Some((branch, base)) = &branch {
                ui.label(
                    RichText::new(format!("⎇ {branch}"))
                        .small()
                        .color(colors.accent.0),
                )
                .on_hover_text(format!("Worktree branch, started from {base}"));
            }
            ui.add(
                egui::Label::new(RichText::new(folder).small().color(colors.text_muted.0))
                    .truncate(),
            );
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
                let streaming = self.runs.get(id).map(|a| a.streaming.as_str());
                transcript(ui, thread, streaming, running, colors);
                if self.creating_worktree(id) {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.spinner();
                        ui.label(
                            RichText::new("Creating worktree…")
                                .small()
                                .color(colors.text_muted.0),
                        );
                    });
                }
                if let Some(active) = self.runs.get(id) {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
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
                    let in_repo = self.in_repo;
                    let base = self.current_branch.clone();
                    let t = self.threads.iter_mut().find(|t| t.id == id).unwrap();
                    // Where the first turn runs: here, or in a new worktree.
                    if in_repo && t.not_started() && !running {
                        let text = if t.use_worktree {
                            "⎇ New worktree"
                        } else {
                            "⎇ Local"
                        };
                        let tip = match (&base, t.use_worktree) {
                            (Some(b), true) => format!(
                                "Runs in a new git worktree on its own branch from {b}, leaving your working copy alone. Click to work in this folder instead."
                            ),
                            (_, true) => "Runs in a new git worktree on its own branch. Click to work in this folder instead.".to_owned(),
                            (_, false) => "Works in this folder. Click to run in a new git worktree on its own branch instead.".to_owned(),
                        };
                        let fill = if t.use_worktree {
                            colors.accent.0.gamma_multiply(0.35)
                        } else {
                            colors.widget.0
                        };
                        if ui
                            .add(
                                egui::Button::new(RichText::new(text).small())
                                    .fill(fill)
                                    .corner_radius(5.0),
                            )
                            .on_hover_text(tip)
                            .clicked()
                        {
                            t.use_worktree = !t.use_worktree;
                            // New threads start the same way.
                            settings.new_thread_worktree = t.use_worktree;
                            self.dirty = true;
                            action = Some(ThreadAction::SettingsChanged);
                        }
                    }
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

/// A run of the transcript drawn as one piece.
#[derive(Debug, PartialEq)]
enum Block<'a> {
    User(&'a str),
    Reply(&'a str),
    /// Consecutive tool calls, shown as one accordion.
    Tools(&'a [Message]),
    Error(&'a str),
    Info(&'a str),
}

fn blocks(messages: &[Message]) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        match &messages[i] {
            Message::Tool { .. } => {
                let start = i;
                while i < messages.len() && matches!(messages[i], Message::Tool { .. }) {
                    i += 1;
                }
                out.push(Block::Tools(&messages[start..i]));
                continue;
            }
            Message::User { text } => out.push(Block::User(text)),
            Message::Assistant { text } => out.push(Block::Reply(text)),
            Message::Error { text } => out.push(Block::Error(text)),
            Message::Info { text } => out.push(Block::Info(text)),
        }
        i += 1;
    }
    out
}

/// "4 tool calls · Read ×2, Bash, Edit · 1 failed"
fn tool_summary(tools: &[Message]) -> String {
    let mut names: Vec<(String, usize)> = Vec::new();
    let mut failed = 0;
    for m in tools {
        if let Message::Tool { name, is_error, .. } = m {
            match names.iter_mut().find(|(n, _)| n == name) {
                Some((_, c)) => *c += 1,
                None => names.push((name.clone(), 1)),
            }
            if *is_error {
                failed += 1;
            }
        }
    }
    let n = tools.len();
    let list: Vec<String> = names
        .iter()
        .map(|(name, c)| {
            if *c > 1 {
                format!("{name} ×{c}")
            } else {
                name.clone()
            }
        })
        .collect();
    let mut text = format!(
        "{n} tool call{} · {}",
        if n == 1 { "" } else { "s" },
        list.join(", ")
    );
    if failed > 0 {
        text.push_str(&format!(" · {failed} failed"));
    }
    text
}

fn transcript(
    ui: &mut Ui,
    thread: &super::thread::Thread,
    streaming: Option<&str>,
    running: bool,
    colors: &UiColors,
) {
    let accent = picker::provider_color(thread.choice.provider, colors);
    let list = blocks(&thread.messages);
    // Each agent turn gets one header, above its first reply or tool use.
    let mut need_header = true;
    let header = |ui: &mut Ui, need: &mut bool| {
        if *need {
            agent_header(ui, thread, colors);
            *need = false;
        }
    };
    let last = list.len().saturating_sub(1);
    let streaming = streaming.filter(|s| !s.trim().is_empty());
    for (i, block) in list.iter().enumerate() {
        match block {
            Block::User(text) => {
                user_bubble(ui, text, colors);
                need_header = true;
            }
            Block::Reply(text) => {
                header(ui, &mut need_header);
                reply(ui, text, accent);
            }
            Block::Tools(tools) => {
                header(ui, &mut need_header);
                // Still running when nothing has come after it yet.
                let live = running && i == last && streaming.is_none();
                tool_group(ui, &thread.id, tools, live, colors);
            }
            Block::Error(text) => {
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0, colors.error.0))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(*text).color(colors.error.0));
                    });
                ui.add_space(4.0);
            }
            Block::Info(text) => {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(RichText::new(*text).small().color(colors.text_muted.0));
                });
            }
        }
    }
    if running {
        header(ui, &mut need_header);
    }
    if let Some(text) = streaming {
        reply(ui, text, accent);
    }
}

/// Provider mark, name and model at the start of an agent's turn.
fn agent_header(ui: &mut Ui, thread: &super::thread::Thread, colors: &UiColors) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
        picker::provider_icon(ui.painter(), rect, thread.choice.provider, colors);
        ui.label(
            RichText::new(thread.choice.provider.name())
                .strong()
                .color(colors.text.0),
        );
        ui.label(
            RichText::new(thread.choice.label())
                .small()
                .color(colors.text_muted.0),
        );
    });
    ui.add_space(2.0);
}

/// The agent's words: Markdown with an accent bar down the left edge.
fn reply(ui: &mut Ui, text: &str, accent: egui::Color32) {
    let rect = egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 14,
            right: 4,
            top: 2,
            bottom: 2,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            crate::ui::markdown::show(ui, text);
        })
        .response
        .rect;
    ui.painter().vline(
        rect.left() + 4.0,
        rect.y_range(),
        egui::Stroke::new(2.5, accent),
    );
    ui.add_space(6.0);
}

/// The user's message, right-aligned in a bubble.
fn user_bubble(ui: &mut Ui, text: &str, colors: &UiColors) {
    ui.add_space(10.0);
    let max = (ui.available_width() * 0.8).max(120.0);
    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
        ui.label(RichText::new("You").small().color(colors.text_muted.0));
        egui::Frame::new()
            .fill(colors.widget.0)
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_max_width(max);
                ui.add(egui::Label::new(RichText::new(text).color(colors.text.0)).wrap());
            });
    });
    ui.add_space(4.0);
}

/// Consecutive tool calls folded into one accordion; each call with output
/// folds again inside it.
fn tool_group(ui: &mut Ui, thread_id: &str, tools: &[Message], live: bool, colors: &UiColors) {
    let first_id = match tools.first() {
        Some(Message::Tool { id, .. }) => id.as_str(),
        _ => "",
    };
    let title = if live {
        match tools.last() {
            Some(Message::Tool { name, detail, .. }) => {
                format!(
                    "Running {name} {}…",
                    super::provider::first_line(detail, 60)
                )
            }
            _ => "Running…".to_owned(),
        }
    } else {
        tool_summary(tools)
    };
    let any_failed = tools
        .iter()
        .any(|m| matches!(m, Message::Tool { is_error: true, .. }));
    let color = if any_failed && !live {
        colors.error.0
    } else {
        colors.text_muted.0
    };
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.vertical(|ui| {
            egui::CollapsingHeader::new(RichText::new(title).small().color(color))
                .id_salt(("tools", thread_id, first_id))
                .default_open(false)
                .show(ui, |ui| {
                    for m in tools {
                        let Message::Tool {
                            id,
                            name,
                            detail,
                            output,
                            is_error,
                        } = m
                        else {
                            continue;
                        };
                        let (glyph, c) = match (output, is_error) {
                            (None, _) if live => ("…", colors.text_muted.0),
                            (None, _) => ("•", colors.text_muted.0),
                            (Some(_), true) => ("✗", colors.error.0),
                            (Some(_), false) => ("✓", colors.added.0),
                        };
                        let line = RichText::new(format!("{glyph} {name}  {detail}"))
                            .small()
                            .monospace()
                            .color(if *is_error { colors.error.0 } else { c });
                        match output.as_deref().filter(|o| !o.trim().is_empty()) {
                            Some(out) => {
                                egui::CollapsingHeader::new(line)
                                    .id_salt(("tool", thread_id, id))
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        egui::Frame::new()
                                            .fill(colors.panel.0)
                                            .corner_radius(4.0)
                                            .inner_margin(egui::Margin::same(6))
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.label(RichText::new(out).small().monospace());
                                            });
                                    });
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    ui.add_space(18.0);
                                    ui.label(line);
                                });
                            }
                        }
                    }
                });
        });
    });
    ui.add_space(2.0);
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

    fn tool(name: &str, err: bool) -> Message {
        Message::Tool {
            id: name.into(),
            name: name.into(),
            detail: String::new(),
            output: Some(String::new()),
            is_error: err,
        }
    }

    #[test]
    fn consecutive_tools_group_together() {
        let msgs = vec![
            Message::User { text: "hi".into() },
            tool("Read", false),
            tool("Read", false),
            tool("Bash", true),
            Message::Assistant {
                text: "done".into(),
            },
            tool("Edit", false),
        ];
        let b = blocks(&msgs);
        assert_eq!(b.len(), 4);
        assert!(matches!(b[1], Block::Tools(t) if t.len() == 3));
        assert_eq!(b[2], Block::Reply("done"));
        assert!(matches!(b[3], Block::Tools(t) if t.len() == 1));
        assert_eq!(
            tool_summary(&msgs[1..4]),
            "3 tool calls · Read ×2, Bash · 1 failed"
        );
        assert_eq!(tool_summary(&msgs[5..]), "1 tool call · Edit");
    }

    #[test]
    fn conflicts_block_everything() {
        let mut g = repo();
        g.changes = 1;
        g.conflicts = 1;
        assert!(g.flows().is_empty());
    }
}

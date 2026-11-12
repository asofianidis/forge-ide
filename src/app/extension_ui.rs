//! Extensions in the app: the sidebar view, running extension commands and
//! applying their output, on-save hooks, and the install and prompt dialogs.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{Id, Key, Modifiers, RichText};

use super::{ForgeApp, open_path};
use crate::editor::Document;
use crate::editor::ops::Selection;
use crate::extensions::install::{self, Package};
use crate::extensions::panel::RunState;
use crate::extensions::run::{self, Context, Invocation, RunOutput};
use crate::extensions::{self, Extensions, Input, Output, PanelAction};
use crate::lsp::text::CharEdit;
use crate::panes::{LogPane, MarkdownPane, Pane};
use crate::task::Pending;
use crate::ui::activity::SidebarView;

/// Extension state in [`ForgeApp`] besides the loaded [`Extensions`].
#[derive(Default)]
pub(super) struct ExtensionUi {
    panel: extensions::ExtensionsPanel,
    runs: Vec<Run>,
    /// Output of each command's last run, by qualified id: (title, output).
    last_output: HashMap<String, (String, RunOutput)>,
    /// Commands chosen from the palette, sidebar or a shortcut, run at the
    /// end of the frame.
    pub(super) pending: Vec<String>,
    prompt: Option<PromptDialog>,
    install: Option<InstallDialog>,
    /// Set while re-saving a file an on-save command rewrote, so the hooks
    /// don't run again.
    saving_from_hook: bool,
}

/// A command running in the background.
struct Run {
    command: String,
    title: String,
    output: Output,
    timeout: Option<Duration>,
    /// Document the command ran on (by id), with its text at the start.
    doc: Option<(u64, String)>,
    /// Chars replaced by `replace-selection` / `insert` output.
    range: Range<usize>,
    /// The replaced text ended with a newline, so the output's last newline
    /// is kept.
    keep_final_newline: bool,
    on_save: bool,
    task: Pending<Result<RunOutput, String>>,
}

struct PromptDialog {
    command: String,
    title: String,
    prompt: String,
    text: String,
    focus: bool,
}

struct InstallDialog {
    package: Result<Package, String>,
    /// Version of the same extension already installed.
    replaces: Option<String>,
}

/// Why a command is being run.
pub(super) enum Trigger {
    /// From the palette, sidebar or a shortcut; `input` answers the prompt.
    Manual { input: Option<String> },
    /// After the document at this index was saved.
    Save(usize),
}

impl ForgeApp {
    pub(super) fn extension_dialog_open(&self) -> bool {
        self.ext.prompt.is_some() || self.ext.install.is_some()
    }

    /// The Extensions sidebar.
    pub(super) fn extensions_view(&mut self, ui: &mut egui::Ui) {
        let running: HashSet<String> = self.ext.runs.iter().map(|r| r.command.clone()).collect();
        let has_output: HashSet<String> = self.ext.last_output.keys().cloned().collect();
        let colors = self.current_theme().ui.clone();
        let runs = RunState {
            running: &running,
            has_output: &has_output,
        };
        if let Some(action) = self.ext.panel.show(ui, &self.extensions, &runs, &colors) {
            self.extension_action(&ui.ctx().clone(), action);
        }
    }

    fn extension_action(&mut self, ctx: &egui::Context, action: PanelAction) {
        match action {
            PanelAction::Install => self.pick_extension_zip(),
            PanelAction::Reload => self.reload_extensions(ctx, true),
            PanelAction::OpenFolder => self.open_extensions_folder(),
            PanelAction::ShowGuide => self.show_extension_guide(),
            PanelAction::Run(id) => self.ext.pending.push(id),
            PanelAction::ShowOutput(id) => self.show_extension_output(&id),
            PanelAction::SetEnabled(id, on) => {
                self.update_settings(|s| {
                    s.disabled_extensions.retain(|d| *d != id);
                    if !on {
                        s.disabled_extensions.push(id.clone());
                    }
                });
                self.reload_extensions(ctx, false);
                let name = self.extensions.get(&id).map_or(id, |e| e.name());
                self.status = format!("{} {name}", if on { "Enabled" } else { "Disabled" });
            }
            PanelAction::Uninstall(id) => self.uninstall_extension(ctx, &id),
            PanelAction::ShowDetails(id) => self.show_extension_details(&id),
            PanelAction::Reveal(path) => {
                if let Err(e) = open_path(&path) {
                    self.status = format!("Error: {e:#}");
                }
            }
            PanelAction::ApplyTheme(name) => {
                match self.themes.iter().position(|t| t.name == name) {
                    Some(idx) => self.set_theme(ctx, idx),
                    None => self.status = format!("Theme {name} isn't loaded"),
                }
            }
        }
    }

    // ---- loading -------------------------------------------------------

    /// Re-reads every extension (and the themes they contribute).
    pub(super) fn reload_extensions(&mut self, ctx: &egui::Context, report: bool) {
        self.extensions = Extensions::load(
            extensions::extensions_dir(),
            &self.settings.disabled_extensions,
            self.keymap.table(),
        );
        self.reload_themes(ctx);
        if !report {
            return;
        }
        let broken: Vec<String> = self
            .extensions
            .list
            .iter()
            .filter(|e| e.error.is_some())
            .map(|e| e.name())
            .collect();
        let n = self.extensions.active_count();
        self.status = match broken.as_slice() {
            [] => format!("Loaded {n} extension{}", if n == 1 { "" } else { "s" }),
            [one] => format!("Loaded {n} extensions; {one} failed to load"),
            more => format!("Loaded {n} extensions; {} failed to load", more.len()),
        };
    }

    pub(super) fn open_extensions_folder(&mut self) {
        let Some(dir) = extensions::extensions_dir() else {
            self.status = "Error: no config directory".to_owned();
            return;
        };
        let result = std::fs::create_dir_all(&dir)
            .map_err(anyhow::Error::from)
            .and_then(|()| open_path(&dir));
        self.status = match result {
            Ok(()) => format!("Extensions folder: {}", dir.display()),
            Err(e) => format!("Error: {e:#}"),
        };
    }

    pub(super) fn show_extension_guide(&mut self) {
        self.open_pane(Pane::Markdown(MarkdownPane::new(
            "Extension Guide".to_owned(),
            "extension-guide".to_owned(),
            extensions::GUIDE.to_owned(),
        )));
    }

    /// Opens a tab describing an extension: what it contributes, then its README.
    fn show_extension_details(&mut self, id: &str) {
        let Some(ext) = self.extensions.get(id) else {
            return;
        };
        let mut md = format!("# {}\n\n", ext.name());
        if let Some(m) = &ext.manifest {
            md.push_str(&format!("`{}` version {}", m.id, m.version));
            if !m.author.is_empty() {
                md.push_str(&format!(" by {}", m.author));
            }
            md.push_str("\n\n");
            if !m.description.is_empty() {
                md.push_str(&format!("{}\n\n", m.description));
            }
            if !m.homepage.is_empty() {
                md.push_str(&format!("[{0}]({0})\n\n", m.homepage));
            }
            if !m.commands.is_empty() {
                md.push_str("## Commands\n\n");
                for c in &m.commands {
                    md.push_str(&format!("- **{}** `{}:{}`", c.title, m.id, c.id));
                    if let Some(k) = &c.keybinding {
                        md.push_str(&format!(" ({k})"));
                    }
                    if c.on_save {
                        md.push_str(" runs on save");
                    }
                    let (program, _) = c.invocation();
                    md.push_str(&format!(" runs `{program}`\n"));
                }
                md.push('\n');
            }
            if !ext.theme_names.is_empty() {
                md.push_str("## Themes\n\n");
                for t in &ext.theme_names {
                    md.push_str(&format!("- {t}\n"));
                }
                md.push('\n');
            }
        }
        if let Some(e) = &ext.error {
            md.push_str(&format!("> **Error:** {e}\n\n"));
        }
        md.push_str(&format!("Installed in `{}`\n\n", ext.dir.display()));
        if let Some(text) = ext.readme().and_then(|p| std::fs::read_to_string(p).ok()) {
            md.push_str("---\n\n");
            md.push_str(&text);
        }
        let title = ext.name();
        self.open_pane(Pane::Markdown(MarkdownPane::new(
            title,
            format!("extension:{id}"),
            md,
        )));
    }

    // ---- install / uninstall ---------------------------------------------

    pub(super) fn pick_extension_zip(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Install Extension")
            .add_filter("Extension package", &["zip"])
            .pick_file()
        {
            self.inspect_extension_zip(&path);
        }
    }

    /// Checks a zip and asks the user to confirm installing it.
    pub(super) fn inspect_extension_zip(&mut self, path: &Path) {
        let package = install::inspect(path);
        let replaces = package.as_ref().ok().and_then(|p| {
            let ext = self.extensions.get(&p.manifest.id)?;
            Some(
                ext.manifest
                    .as_ref()
                    .map_or_else(String::new, |m| m.version.clone()),
            )
        });
        self.ext.install = Some(InstallDialog { package, replaces });
    }

    /// Offers to install `.zip` files dropped onto the window.
    pub(super) fn handle_dropped_zips(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
                .collect()
        });
        if let Some(path) = dropped.first() {
            self.inspect_extension_zip(path);
        }
    }

    fn install_package(&mut self, ctx: &egui::Context, package: &Package) {
        let Some(dir) = extensions::extensions_dir() else {
            self.status = "Error: no config directory".to_owned();
            return;
        };
        match install::install(package, &dir) {
            Ok(_) => {
                self.reload_extensions(ctx, false);
                let m = &package.manifest;
                self.status = match self.extensions.get(&m.id) {
                    Some(e) if e.error.is_some() => format!(
                        "Installed {}, but it failed to load: {}",
                        m.name,
                        e.error.as_deref().unwrap_or_default()
                    ),
                    Some(e) if !e.enabled => {
                        format!("Installed {} {} (disabled)", m.name, m.version)
                    }
                    _ => format!("Installed {} {}", m.name, m.version),
                };
                self.sidebar_view = SidebarView::Extensions;
                self.sidebar_open = true;
            }
            Err(e) => {
                self.status = format!("Error: couldn't install {}: {e}", package.manifest.name)
            }
        }
    }

    fn uninstall_extension(&mut self, ctx: &egui::Context, id: &str) {
        let (Some(ext), Some(dir)) = (self.extensions.get(id), self.extensions.dir.as_deref())
        else {
            return;
        };
        let name = ext.name();
        match install::uninstall(&ext.dir, dir) {
            Ok(()) => {
                self.update_settings(|s| s.disabled_extensions.retain(|d| d != id));
                self.reload_extensions(ctx, false);
                self.status = format!("Uninstalled {name}");
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
    }

    /// The install confirmation (or error) dialog.
    pub(super) fn extension_install_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &self.ext.install else {
            return;
        };
        let colors = self.current_theme().ui.clone();
        let mut install = false;
        let mut close = false;
        let modal = egui::Modal::new(Id::unique("extension_install")).show(ctx, |ui| {
            ui.set_width(440.0);
            let pkg = match &dialog.package {
                Ok(p) => p,
                Err(e) => {
                    ui.heading("Can't install this extension");
                    ui.add_space(4.0);
                    ui.colored_label(colors.error.0, e);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.link("How extensions are packaged").clicked() {
                            self.pending_commands
                                .push(crate::commands::Command::ExtensionGuide);
                            close = true;
                        }
                    });
                    ui.add_space(8.0);
                    close |= ui.button("Close").clicked();
                    return;
                }
            };
            let m = &pkg.manifest;
            ui.heading(format!("Install {}?", m.name));
            let mut by = format!("{} {}", m.id, m.version);
            if !m.author.is_empty() {
                by.push_str(&format!(" by {}", m.author));
            }
            ui.label(RichText::new(by).color(colors.text_muted.0));
            if !m.description.is_empty() {
                ui.add_space(4.0);
                ui.label(&m.description);
            }
            ui.add_space(8.0);
            match &dialog.replaces {
                Some(v) if v == &m.version => {
                    ui.label(format!(
                        "Reinstalls version {v}, which is already installed."
                    ));
                }
                Some(v) if !v.is_empty() => {
                    ui.label(format!("Replaces the installed version {v}."));
                }
                Some(_) => {
                    ui.label("Replaces the installed copy.");
                }
                None => {}
            }
            let mut parts = Vec::new();
            if !m.commands.is_empty() {
                parts.push(format!(
                    "{} command{}",
                    m.commands.len(),
                    if m.commands.len() == 1 { "" } else { "s" }
                ));
            }
            if !m.themes.is_empty() {
                parts.push(format!(
                    "{} theme{}",
                    m.themes.len(),
                    if m.themes.len() == 1 { "" } else { "s" }
                ));
            }
            if parts.is_empty() {
                ui.label("Adds nothing yet.");
            } else {
                ui.label(format!("Adds {}.", parts.join(" and ")));
            }
            ui.label(
                RichText::new(format!(
                    "{} file{}, {}",
                    pkg.files,
                    if pkg.files == 1 { "" } else { "s" },
                    human_size(pkg.bytes)
                ))
                .small()
                .color(colors.text_muted.0),
            );
            let programs = m.programs();
            if !programs.is_empty() {
                ui.label(format!("Runs: {}", programs.join(", ")));
            }
            let on_save: Vec<&str> = m
                .commands
                .iter()
                .filter(|c| c.on_save)
                .map(|c| c.title.as_str())
                .collect();
            if !on_save.is_empty() {
                ui.label(format!(
                    "Runs automatically on save: {}",
                    on_save.join(", ")
                ));
            }
            ui.add_space(8.0);
            ui.colored_label(
                colors.warning.0,
                "Extensions run programs on your computer with your permissions. \
                 Only install extensions from people you trust.",
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                install = ui.button("Install").clicked();
                close |= ui.button("Cancel").clicked();
            });
        });
        if modal.should_close() {
            close = true;
        }
        if install
            && let Some(InstallDialog {
                package: Ok(pkg), ..
            }) = self.ext.install.take()
        {
            self.install_package(ctx, &pkg);
        } else if close {
            self.ext.install = None;
        }
    }

    // ---- running ---------------------------------------------------------

    /// Runs commands queued this frame.
    pub(super) fn run_pending_extension_commands(&mut self, ctx: &egui::Context) {
        for id in std::mem::take(&mut self.ext.pending) {
            self.run_extension_command(ctx, &id, Trigger::Manual { input: None });
        }
    }

    pub(super) fn run_extension_command(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        trigger: Trigger,
    ) {
        let Some((ext, spec)) = self.extensions.command(id) else {
            self.status = format!("Extension command {id} isn't available");
            return;
        };
        let spec = spec.clone();
        let extension_dir = ext.dir.clone();
        let title = spec.title.clone();
        let (doc_index, input, manual) = match trigger {
            Trigger::Manual { input } => {
                if input.is_none()
                    && let Some(prompt) = &spec.prompt
                {
                    self.ext.prompt = Some(PromptDialog {
                        command: id.to_owned(),
                        title: title.clone(),
                        prompt: prompt.clone(),
                        text: String::new(),
                        focus: true,
                    });
                    return;
                }
                // A diff or PR tab has no editor to act on.
                (
                    self.active_pane.is_none().then_some(self.active),
                    input,
                    true,
                )
            }
            Trigger::Save(idx) => (Some(idx), None, false),
        };
        let doc = doc_index.and_then(|i| self.documents.get(i));
        let needs_doc = spec.output.edits_text() || spec.stdin != Input::None;
        if needs_doc && doc.is_none() {
            self.status = format!("{title}: switch to an editor tab first");
            return;
        }
        let file_name = doc
            .and_then(|d| d.path.as_deref())
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned());
        if !spec.applies_to(file_name.as_deref()) {
            self.status = format!("{title} only works on {} files", spec.file_types.join(", "));
            return;
        }
        let sel = if manual && doc_index == Some(self.active) {
            self.selection(ctx)
        } else {
            Selection::caret(0)
        };
        let text = doc.map(Document::text).unwrap_or_default();
        let selection: String = text
            .chars()
            .skip(sel.start)
            .take(sel.end - sel.start)
            .collect();
        let (line, column) = doc.map_or((1, 1), |d| d.line_col(sel.end));
        let context = Context {
            file: doc.and_then(|d| d.path.clone()),
            workspace: self.workspace.as_ref().map(|w| w.root.path.clone()),
            extension_dir,
            selection: selection.clone(),
            document: if spec.stdin == Input::Document {
                text.to_owned()
            } else {
                String::new()
            },
            line,
            column,
            input,
        };
        let invocation = match Invocation::new(&spec, &context) {
            Ok(i) => i,
            Err(e) => {
                self.status = format!("{title}: {e}");
                return;
            }
        };
        let doc_state = doc.map(|d| (d.id, d.text().to_owned()));
        let doc_id = doc_state.as_ref().map(|(id, _)| *id);
        if self
            .ext
            .runs
            .iter()
            .any(|r| r.command == id && r.doc.as_ref().map(|(i, _)| *i) == doc_id)
        {
            if manual {
                self.status = format!("{title} is already running");
            }
            return;
        }
        let range = match spec.output {
            Output::Insert => sel.end..sel.end,
            _ => sel.start..sel.end,
        };
        let timeout = invocation.timeout;
        let task = Pending::spawn(ctx, move || run::run(&invocation));
        if manual && spec.output != Output::Silent {
            self.status = format!("Running {title}…");
        }
        self.ext.runs.push(Run {
            command: id.to_owned(),
            title,
            output: spec.output,
            timeout,
            doc: doc_state,
            range,
            keep_final_newline: selection.ends_with('\n'),
            on_save: !manual,
            task,
        });
    }

    /// Runs the on-save commands that apply to the document just saved.
    pub(super) fn run_save_hooks(&mut self, idx: usize) {
        if self.ext.saving_from_hook {
            return;
        }
        let Some(name) = self
            .documents
            .get(idx)
            .and_then(|d| d.path.as_deref())
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return;
        };
        let hooks: Vec<String> = self
            .extensions
            .commands()
            .filter(|(_, c, _)| c.on_save && c.applies_to(Some(&name)))
            .map(|(_, _, id)| id)
            .collect();
        let ctx = self.egui_ctx.clone();
        for id in hooks {
            self.run_extension_command(&ctx, &id, Trigger::Save(idx));
        }
    }

    /// Picks up finished commands and acts on their output.
    pub(super) fn poll_extension_runs(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.ext.runs.len() {
            match self.ext.runs[i].task.try_take() {
                Some(result) => {
                    let run = self.ext.runs.remove(i);
                    self.finish_extension_run(ctx, run, result);
                }
                None => i += 1,
            }
        }
    }

    fn finish_extension_run(
        &mut self,
        ctx: &egui::Context,
        run: Run,
        result: Result<RunOutput, String>,
    ) {
        let output = match result {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("Error: {}: {e}", run.title);
                let failed = RunOutput {
                    stderr: e,
                    ..RunOutput::default()
                };
                self.ext
                    .last_output
                    .insert(run.command.clone(), (run.title.clone(), failed));
                return;
            }
        };
        self.ext
            .last_output
            .insert(run.command.clone(), (run.title.clone(), output.clone()));
        if !output.success {
            self.status = format!(
                "Error: {}: {}",
                run.title,
                output.error_summary(run.timeout)
            );
            if run.output == Output::Panel {
                self.show_extension_output(&run.command);
            }
            return;
        }
        match run.output {
            Output::Status => {
                let line = output.stdout.lines().map(str::trim).find(|l| !l.is_empty());
                self.status = match line {
                    Some(l) => l.chars().take(300).collect(),
                    None if run.on_save => return,
                    None => format!("{}: done", run.title),
                };
            }
            Output::Silent => {}
            Output::Panel => self.show_extension_output(&run.command),
            Output::NewDocument => {
                let mut doc = Document::untitled();
                doc.set_text(&output.stdout);
                doc.buffer.mark_saved();
                self.documents.push(doc);
                self.active = self.documents.len() - 1;
                self.active_pane = None;
            }
            Output::ReplaceDocument | Output::ReplaceSelection | Output::Insert => {
                self.apply_extension_edit(ctx, &run, output.stdout);
            }
        }
    }

    /// Puts a command's output into the document it ran on, unless the
    /// document changed in the meantime.
    fn apply_extension_edit(&mut self, ctx: &egui::Context, run: &Run, mut text: String) {
        let Some((doc_id, original)) = &run.doc else {
            return;
        };
        let Some(idx) = self.documents.iter().position(|d| d.id == *doc_id) else {
            self.status = format!("{}: the tab was closed before it finished", run.title);
            return;
        };
        if self.documents[idx].text() != original {
            self.status = format!(
                "{}: the file changed while it ran, so its result was discarded",
                run.title
            );
            return;
        }
        if run.output == Output::ReplaceDocument {
            if text == *original {
                if !run.on_save {
                    self.status = format!("{}: no changes", run.title);
                }
                return;
            }
            let edit = whole_text_edit(original, &text);
            self.edit_document(ctx, idx, &[edit]);
            if run.on_save {
                self.ext.saving_from_hook = true;
                self.save(idx);
                self.ext.saving_from_hook = false;
            } else {
                self.status = format!("{}: done", run.title);
            }
            return;
        }
        // Programs usually end their output with a newline; don't add one
        // the replaced text didn't have.
        if !run.keep_final_newline {
            if let Some(t) = text.strip_suffix("\r\n") {
                text.truncate(t.len());
            } else if let Some(t) = text.strip_suffix('\n') {
                text.truncate(t.len());
            }
        }
        let start = run.range.start;
        let end = start + text.chars().count();
        self.documents[idx].apply_edits([(run.range.clone(), text.as_str())]);
        if idx == self.active && self.active_pane.is_none() {
            let sel = if run.output == Output::Insert {
                Selection::caret(end)
            } else {
                Selection::new(start, end)
            };
            self.set_selection(ctx, sel, true);
            self.scroll_to = None;
            self.reveal = Some(end);
        }
    }

    pub(super) fn show_extension_output(&mut self, id: &str) {
        let Some((title, output)) = self.ext.last_output.get(id) else {
            return;
        };
        let text = output.combined();
        let pane = LogPane::finished(
            format!("Output: {title}"),
            format!("extension-output:{id}"),
            Ok(if text.is_empty() {
                "(no output)".to_owned()
            } else {
                text
            }),
        );
        // Replace the old output rather than showing it again.
        let key = format!("extension-output:{id}");
        if let Some(i) = self
            .panes
            .iter()
            .position(|p| matches!(p, Pane::Log(l) if l.key == key))
        {
            self.close_pane(i);
        }
        self.open_pane(Pane::Log(pane));
    }

    /// Whether any extension command is running (for the status bar).
    pub(super) fn extension_spinner(&self, ui: &mut egui::Ui) {
        if let Some(run) = self.ext.runs.first() {
            let tip = match self.ext.runs.len() {
                1 => format!("Running {}", run.title),
                n => format!("Running {} extension commands", n),
            };
            ui.spinner().on_hover_text(tip);
        }
    }

    /// The dialog asking for a command's `${input}`.
    pub(super) fn extension_prompt_dialog(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.ext.prompt else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let modal = egui::Modal::new(Id::unique("extension_prompt")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(&prompt.title);
            ui.label(&prompt.prompt);
            let field =
                ui.add(egui::TextEdit::singleline(&mut prompt.text).desired_width(f32::INFINITY));
            if std::mem::take(&mut prompt.focus) {
                field.request_focus();
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                submit = true;
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                submit |= ui.button("Run").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if modal.should_close() || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            cancel = true;
        }
        if submit && let Some(p) = self.ext.prompt.take() {
            self.run_extension_command(
                ctx,
                &p.command,
                Trigger::Manual {
                    input: Some(p.text),
                },
            );
        } else if cancel {
            self.ext.prompt = None;
        }
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / 1024.0 / 1024.0),
    }
}

/// One edit turning `old` into `new`, covering only the part that differs,
/// so the caret outside it stays put.
fn whole_text_edit(old: &str, new: &str) -> CharEdit {
    let prefix = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let old_len = old.chars().count();
    let new_len = new.chars().count();
    let max_suffix = old_len.min(new_len) - prefix;
    let suffix = old
        .chars()
        .rev()
        .zip(new.chars().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    CharEdit {
        range: prefix..old_len - suffix,
        text: new
            .chars()
            .skip(prefix)
            .take(new_len - suffix - prefix)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::whole_text_edit;

    fn apply(old: &str, new: &str) -> String {
        let e = whole_text_edit(old, new);
        let mut chars: Vec<char> = old.chars().collect();
        chars.splice(e.range, e.text.chars());
        chars.into_iter().collect()
    }

    #[test]
    fn whole_text_edit_covers_only_the_change() {
        let e = whole_text_edit("fn a() {}\nfn b() {}\n", "fn a() {}\nfn  b() {}\n");
        assert_eq!(e.range, 13..13);
        assert_eq!(e.text, " ");
        for (old, new) in [
            ("aaa", "aa"),
            ("aa", "aaa"),
            ("", "x"),
            ("x", ""),
            ("héllo", "hello"),
            ("abcabc", "abc"),
        ] {
            assert_eq!(apply(old, new), new);
        }
    }
}

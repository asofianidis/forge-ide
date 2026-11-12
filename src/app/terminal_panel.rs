//! The integrated terminal panel below the editor.

use std::time::Duration;

use egui::{Key, Modifiers, RichText, TextStyle};

use super::ForgeApp;
use crate::commands::Command;
use crate::settings::Settings;
use crate::terminal::Terminal;
use crate::ui::icons::{self, Icon};

/// The terminal panel's state in [`ForgeApp`].
#[derive(Default)]
pub(super) struct TerminalPanel {
    /// The running (or exited) shell; `None` until the panel is first opened.
    terminal: Option<Terminal>,
    pub(super) open: bool,
    /// Give the terminal keyboard focus on the next frame.
    focus: bool,
}

impl ForgeApp {
    /// Whether the terminal has keyboard focus, so its keys go to the shell.
    pub(super) fn terminal_has_focus(&self, ctx: &egui::Context) -> bool {
        self.terminal.open && ctx.memory(|m| m.has_focus(Terminal::id()))
    }

    /// Opens the panel (focusing the shell), or hides it if the shell already
    /// has focus.
    pub(super) fn toggle_terminal(&mut self, ctx: &egui::Context) {
        if self.terminal_has_focus(ctx) {
            self.terminal.open = false;
        } else {
            self.start_terminal(ctx);
        }
    }

    /// Kills the current shell and starts a new one.
    pub(super) fn restart_terminal(&mut self, ctx: &egui::Context) {
        self.terminal.terminal = None;
        self.start_terminal(ctx);
    }

    /// Shows the panel, starts the shell if it isn't running, and focuses it.
    fn start_terminal(&mut self, ctx: &egui::Context) {
        self.terminal.open = true;
        if self.terminal.terminal.is_none() {
            // Re-read the settings file so a hand-edited `terminal_shell`
            // applies on the next (re)start without restarting Forge.
            self.settings.terminal_shell = Settings::load().terminal_shell;
            let cwd = self.workspace.as_ref().map(|ws| ws.root.path.as_path());
            match Terminal::spawn(self.settings.terminal_shell.as_deref(), cwd, ctx) {
                Ok(t) => self.terminal.terminal = Some(t),
                Err(e) => self.status = format!("Could not start the terminal: {e:#}"),
            }
        }
        self.terminal.focus = true;
    }

    pub(super) fn terminal_panel(&mut self, ui: &mut egui::Ui) {
        let colors = self.current_theme().ui.clone();
        let exited = self
            .terminal
            .terminal
            .as_mut()
            .is_none_or(Terminal::has_exited);
        if !exited {
            // The PTY doesn't always report EOF when the shell exits, so poll for it.
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        let mut hide = false;
        let mut restart = false;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("TERMINAL")
                    .small()
                    .strong()
                    .color(colors.text_muted.0),
            );
            if exited {
                ui.weak("— process exited");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hide_tip = match self.keymap.shortcut(Command::ToggleTerminal) {
                    Some(s) => format!("Hide ({})", ui.ctx().format_shortcut(s)),
                    None => "Hide".to_owned(),
                };
                hide = icons::button(ui, Icon::Close, &hide_tip).clicked();
                restart = icons::button(ui, Icon::Refresh, "Restart shell").clicked();
            });
        });

        let font = TextStyle::Monospace.resolve(ui.style());
        let focus = std::mem::take(&mut self.terminal.focus);
        match &mut self.terminal.terminal {
            Some(terminal) => terminal.ui(ui, &font, focus, &colors),
            None => {
                ui.weak("The shell could not be started. See the status bar for details.");
            }
        }
        if exited
            && self.terminal_has_focus(ui.ctx())
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
        {
            restart = true;
        }
        if restart {
            self.restart_terminal(&ui.ctx().clone());
        }
        if hide {
            self.terminal.open = false;
        }
    }
}

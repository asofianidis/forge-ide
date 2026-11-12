//! Integrated terminal: a shell running in a pseudo-terminal, rendered from a
//! `vt100` screen grid.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, Event, EventFilter, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, pos2, vec2,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::theme::UiColors;

const SCROLLBACK_LINES: usize = 5000;

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// A running shell session.
pub struct Terminal {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: SharedWriter,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Set by the reader thread when the PTY closes.
    closed: Arc<AtomicBool>,
    exit_code: Option<u32>,
    size: (u16, u16),
    /// Fractional lines of pending mouse-wheel scroll.
    scroll_accum: f32,
}

/// The shell to launch: the configured one, or the platform default.
fn shell_command(shell: Option<&str>) -> CommandBuilder {
    match shell.map(str::trim).filter(|s| !s.is_empty()) {
        Some(shell) => {
            let mut parts = shell.split_whitespace();
            let mut cmd = CommandBuilder::new(parts.next().unwrap_or(shell));
            cmd.args(parts);
            cmd
        }
        None if cfg!(windows) => {
            let mut cmd = CommandBuilder::new("powershell.exe");
            cmd.arg("-NoLogo");
            cmd
        }
        None => CommandBuilder::new_default_prog(),
    }
}

impl Terminal {
    /// Widget id of the terminal area, so the app can tell when it has focus.
    pub fn id() -> Id {
        Id::unique("integrated_terminal")
    }

    pub fn spawn(
        shell: Option<&str>,
        cwd: Option<&Path>,
        ctx: &egui::Context,
    ) -> anyhow::Result<Self> {
        let size = (24, 80);
        let pair = native_pty_system()
            .openpty(pty_size(size))
            .context("failed to open a pseudo-terminal")?;
        let mut cmd = shell_command(shell);
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        cmd.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to start the shell")?;
        // The child holds its own handle; ours must be closed so EOF is seen on exit.
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            nz(size.0),
            nz(size.1),
            SCROLLBACK_LINES,
        )));
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let mut reader = pair.master.try_clone_reader()?;
        let closed = Arc::new(AtomicBool::new(false));

        {
            let parser = parser.clone();
            let writer = writer.clone();
            let closed = closed.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("terminal-reader".into())
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    let mut tail = Vec::new();
                    while let Ok(n @ 1..) = reader.read(&mut buf) {
                        let bytes = &buf[..n];
                        let mut parser = parser.lock().unwrap_or_else(|e| e.into_inner());
                        parser.process(bytes);
                        // Answer cursor-position queries (ConPTY sends one on startup
                        // and waits for the reply); vt100 doesn't respond itself.
                        if count_cursor_queries(&mut tail, bytes) > 0 {
                            let (row, col) = parser.screen().cursor_position();
                            let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                            if let Ok(mut w) = writer.lock() {
                                let _ = w.write_all(reply.as_bytes());
                            }
                        }
                        drop(parser);
                        ctx.request_repaint();
                    }
                    closed.store(true, Ordering::Relaxed);
                    ctx.request_repaint();
                })?;
        }

        Ok(Self {
            parser,
            writer,
            master: pair.master,
            child,
            closed,
            exit_code: None,
            size,
            scroll_accum: 0.0,
        })
    }

    /// Whether the shell process has exited.
    pub fn has_exited(&mut self) -> bool {
        if self.exit_code.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            let code = status.exit_code();
            self.exit_code = Some(code);
            let notice = format!(
                "\r\n\x1b[2m[process exited with code {code}; press Enter to restart]\x1b[0m"
            );
            if let Ok(mut p) = self.parser.lock() {
                p.process(notice.as_bytes());
            }
        }
        self.exit_code.is_some() || self.closed.load(Ordering::Relaxed)
    }

    fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) == self.size || rows == 0 || cols == 0 {
            return;
        }
        self.size = (rows, cols);
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(nz(rows), nz(cols));
        }
        let _ = self.master.resize(pty_size(self.size));
    }

    /// Renders the terminal into the available space and forwards input to the shell.
    pub fn ui(&mut self, ui: &mut egui::Ui, font: &FontId, focus: bool, colors: &UiColors) {
        let (cell_w, cell_h) = ui.fonts_mut(|f| (f.glyph_width(font, 'M'), f.row_height(font)));
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        let id = Self::id();
        let response = ui.interact(rect, id, Sense::click());
        if focus || response.clicked() {
            response.request_focus();
        }
        let has_focus = response.has_focus();
        if has_focus {
            // Keep Tab, arrows and Esc for the shell instead of egui focus navigation.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
        }

        let rows = ((rect.height() / cell_h).floor() as u16).max(1);
        let cols = ((rect.width() / cell_w).floor() as u16).max(1);
        self.resize(rows, cols);

        if response.hovered() {
            self.scroll(ui.input(|i| i.smooth_scroll_delta.y) / cell_h);
        }
        if has_focus {
            self.handle_input(ui);
        }

        let default_fg = colors.text.0;
        let default_bg = colors.editor.0;
        let cursor_color = colors.cursor.0;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, default_bg);

        let parser = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        let screen = parser.screen();
        for row in 0..rows {
            let y = rect.top() + f32::from(row) * cell_h;
            let mut job = LayoutJob::default();
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    break;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = to_color(cell.fgcolor(), default_fg, cell.bold());
                let mut bg = to_color(cell.bgcolor(), default_bg, false);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.dim() {
                    fg = fg.gamma_multiply(0.6);
                }
                if bg != default_bg {
                    let x = rect.left() + f32::from(col) * cell_w;
                    let w = if cell.is_wide() { 2.0 } else { 1.0 } * cell_w;
                    painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(w, cell_h)), 0.0, bg);
                }
                let mut format = TextFormat::simple(font.clone(), fg);
                format.italics = cell.italic();
                if cell.underline() {
                    format.underline = Stroke::new(1.0, fg);
                }
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                job.append(text, 0.0, format);
            }
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(pos2(rect.left(), y), galley, default_fg);
        }

        if screen.scrollback() == 0 && !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            let cursor = Rect::from_min_size(
                pos2(
                    rect.left() + f32::from(col) * cell_w,
                    rect.top() + f32::from(row) * cell_h,
                ),
                vec2(cell_w, cell_h),
            );
            if has_focus {
                painter.rect_filled(cursor, 0.0, cursor_color.gamma_multiply(0.6));
            } else {
                painter.rect_stroke(
                    cursor,
                    0.0,
                    Stroke::new(1.0, cursor_color),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    fn scroll(&mut self, lines: f32) {
        self.scroll_accum += lines;
        let whole = self.scroll_accum.trunc();
        if whole == 0.0 {
            return;
        }
        self.scroll_accum -= whole;
        if let Ok(mut p) = self.parser.lock() {
            let current = p.screen().scrollback() as i64;
            let target = (current + whole as i64).max(0) as usize;
            p.screen_mut().set_scrollback(target);
        }
    }

    fn handle_input(&mut self, ui: &egui::Ui) {
        let (app_cursor, bracketed_paste) = {
            let p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
            (
                p.screen().application_cursor(),
                p.screen().bracketed_paste(),
            )
        };
        let events = ui.input(|i| i.events.clone());
        let mut out = Vec::new();
        for event in events {
            match event {
                Event::Text(text) => out.extend_from_slice(text.as_bytes()),
                Event::Paste(text) => {
                    if bracketed_paste {
                        out.extend_from_slice(b"\x1b[200~");
                        out.extend_from_slice(text.as_bytes());
                        out.extend_from_slice(b"\x1b[201~");
                    } else {
                        out.extend_from_slice(
                            text.replace("\r\n", "\r").replace('\n', "\r").as_bytes(),
                        );
                    }
                }
                // egui turns Ctrl+C / Ctrl+X into clipboard events; send the control
                // codes. On macOS those events come from Cmd+C / Cmd+X, while Ctrl+C
                // arrives as a key press (handled by `encode_key`).
                Event::Copy if !cfg!(target_os = "macos") => out.push(0x03),
                Event::Cut if !cfg!(target_os = "macos") => out.push(0x18),
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(bytes) = encode_key(key, modifiers, app_cursor) {
                        out.extend_from_slice(&bytes);
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            // Typing jumps back to the live screen.
            if let Ok(mut p) = self.parser.lock() {
                p.screen_mut().set_scrollback(0);
            }
            self.write(&out);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

fn pty_size((rows, cols): (u16, u16)) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Counts `ESC [ 6 n` (device status report: cursor position) in a byte stream,
/// carrying a partial sequence over between reads in `tail`.
fn count_cursor_queries(tail: &mut Vec<u8>, bytes: &[u8]) -> usize {
    const QUERY: &[u8] = b"\x1b[6n";
    tail.extend_from_slice(bytes);
    let count = tail.windows(QUERY.len()).filter(|w| *w == QUERY).count();
    // Keep just enough to complete a query split across reads.
    let keep = tail.len().min(QUERY.len() - 1);
    tail.drain(..tail.len() - keep);
    count
}

/// Bytes a terminal sends for a key press, or `None` if it produces none (or
/// arrives as [`Event::Text`] instead).
fn encode_key(key: Key, modifiers: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    // Ctrl+letter → C0 control code.
    if modifiers.ctrl && !modifiers.alt {
        let name = key.name();
        if name.len() == 1 {
            let c = name.as_bytes()[0].to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return Some(vec![c - b'a' + 1]);
            }
        }
        match key {
            Key::OpenBracket => return Some(vec![0x1b]),
            Key::Backslash => return Some(vec![0x1c]),
            Key::CloseBracket => return Some(vec![0x1d]),
            Key::Space => return Some(vec![0x00]),
            _ => {}
        }
    }
    let arrow = |c: u8| {
        if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let bytes = match key {
        Key::Enter => vec![b'\r'],
        Key::Backspace => vec![0x7f],
        Key::Tab if modifiers.shift => b"\x1b[Z".to_vec(),
        Key::Tab => vec![b'\t'],
        Key::Escape => vec![0x1b],
        Key::ArrowUp => arrow(b'A'),
        Key::ArrowDown => arrow(b'B'),
        Key::ArrowRight => arrow(b'C'),
        Key::ArrowLeft => arrow(b'D'),
        Key::Home => arrow(b'H'),
        Key::End => arrow(b'F'),
        Key::Insert => b"\x1b[2~".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

/// The standard xterm 16-color palette.
const ANSI: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (13, 188, 121),
    (229, 229, 16),
    (36, 114, 200),
    (188, 63, 188),
    (17, 168, 205),
    (229, 229, 229),
    (102, 102, 102),
    (241, 76, 76),
    (35, 209, 139),
    (245, 245, 67),
    (59, 142, 234),
    (214, 112, 214),
    (41, 184, 219),
    (255, 255, 255),
];

fn to_color(color: vt100::Color, default: Color32, bold: bool) -> Color32 {
    match color {
        vt100::Color::Default => default,
        // Bold text in one of the 8 base colors uses its bright variant.
        vt100::Color::Idx(i) if bold && i < 8 => indexed(i + 8),
        vt100::Color::Idx(i) => indexed(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// xterm 256-color palette: 16 ANSI colors, a 6×6×6 cube, then 24 grays.
fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => {
            let (r, g, b) = ANSI[usize::from(i)];
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_letters_map_to_control_codes() {
        assert_eq!(encode_key(Key::C, Modifiers::CTRL, false), Some(vec![3]));
        assert_eq!(encode_key(Key::A, Modifiers::CTRL, false), Some(vec![1]));
        assert_eq!(encode_key(Key::Z, Modifiers::CTRL, false), Some(vec![26]));
    }

    #[test]
    fn plain_letters_come_from_text_events() {
        assert_eq!(encode_key(Key::A, Modifiers::NONE, false), None);
    }

    #[test]
    fn arrows_respect_application_cursor_mode() {
        assert_eq!(
            encode_key(Key::ArrowUp, Modifiers::NONE, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(Key::ArrowUp, Modifiers::NONE, true),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn editing_keys() {
        assert_eq!(
            encode_key(Key::Enter, Modifiers::NONE, false),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode_key(Key::Backspace, Modifiers::NONE, false),
            Some(vec![0x7f])
        );
        assert_eq!(
            encode_key(Key::Tab, Modifiers::SHIFT, false),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            encode_key(Key::Delete, Modifiers::NONE, false),
            Some(b"\x1b[3~".to_vec())
        );
    }

    #[test]
    fn palette_colors() {
        assert_eq!(indexed(1), Color32::from_rgb(205, 49, 49));
        assert_eq!(indexed(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(indexed(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(
            to_color(vt100::Color::Idx(1), Color32::RED, true),
            indexed(9)
        );
        assert_eq!(
            to_color(vt100::Color::Default, Color32::RED, false),
            Color32::RED
        );
    }

    #[test]
    fn finds_cursor_queries_split_across_reads() {
        let mut tail = Vec::new();
        assert_eq!(count_cursor_queries(&mut tail, b"abc\x1b[6nxyz"), 1);
        assert_eq!(count_cursor_queries(&mut tail, b"hello\x1b["), 0);
        assert_eq!(count_cursor_queries(&mut tail, b"6n"), 1);
        assert_eq!(count_cursor_queries(&mut tail, b"6n"), 0);
    }

    #[test]
    fn vt100_renders_colored_output() {
        let mut parser = vt100::Parser::new(nz(2), nz(10), 0);
        parser.process(b"\x1b[31mred\x1b[0m ok");
        let cell = parser.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "r");
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
        assert_eq!(parser.screen().contents(), "red ok");
    }

    #[test]
    fn custom_shell_command_splits_args() {
        let cmd = shell_command(Some("bash --login"));
        let argv: Vec<_> = cmd
            .get_argv()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, ["bash", "--login"]);
    }

    /// Runs a real command in a PTY and checks its output and exit code.
    #[cfg(unix)]
    #[test]
    fn runs_a_command_and_reports_its_exit() {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir();
        let mut terminal = Terminal::spawn(Some("echo forge-ok"), Some(&dir), &ctx).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !(terminal.has_exited() && terminal.closed.load(Ordering::Relaxed)) {
            assert!(std::time::Instant::now() < deadline, "shell did not exit");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(terminal.exit_code, Some(0));
        let contents = terminal.parser.lock().unwrap().screen().contents();
        assert!(contents.contains("forge-ok"), "{contents:?}");
        assert!(contents.contains("exited with code 0"), "{contents:?}");
    }
}

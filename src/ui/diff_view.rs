//! Renders parsed unified diffs: one collapsible section per file, hunks
//! with old/new line numbers and colored added/removed lines.
//!
//! Callers customize it through [`DiffHooks`]: buttons on file and hunk
//! headers (stage hunk, open file, …) and content under individual lines
//! (inline review comments).

use egui::{Align2, Color32, CursorIcon, FontId, Rect, RichText, Sense, TextStyle, Ui, pos2, vec2};

use crate::git::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
use crate::theme::UiColors;

/// Which side of the diff a line belongs to, as GitHub names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The old file (removed lines).
    Left,
    /// The new file (added and context lines).
    Right,
}

impl Side {
    pub fn api_name(self) -> &'static str {
        match self {
            Side::Left => "LEFT",
            Side::Right => "RIGHT",
        }
    }
}

/// A line's address for commenting: GitHub uses the new line number for
/// added/context lines and the old one for removed lines.
pub fn line_anchor(line: &DiffLine) -> Option<(Side, u32)> {
    match line.kind {
        LineKind::Removed => line.old_no.map(|n| (Side::Left, n)),
        _ => line.new_no.map(|n| (Side::Right, n)),
    }
}

pub trait DiffHooks {
    /// Extra widgets on a file header (right-aligned).
    fn file_actions(&mut self, _ui: &mut Ui, _file_idx: usize, _file: &FileDiff) {}
    /// Extra widgets on a hunk header (right-aligned).
    fn hunk_actions(&mut self, _ui: &mut Ui, _file_idx: usize, _hunk_idx: usize, _file: &FileDiff) {
    }
    /// Whether clicking a line does something (e.g. starts a comment).
    fn lines_clickable(&self) -> bool {
        false
    }
    fn line_clicked(&mut self, _file: &FileDiff, _line: &DiffLine) {}
    /// Content drawn under a line, such as a comment thread.
    fn after_line(&mut self, _ui: &mut Ui, _file: &FileDiff, _line: &DiffLine) {}
    /// Content drawn at the top of a file section (e.g. outdated comments).
    fn file_preamble(&mut self, _ui: &mut Ui, _file: &FileDiff) {}
}

/// Hooks that add nothing.
pub struct NoHooks;
impl DiffHooks for NoHooks {}

pub struct DiffColors {
    pub added_bg: Color32,
    pub removed_bg: Color32,
    pub added_fg: Color32,
    pub removed_fg: Color32,
    pub modified_fg: Color32,
    pub hunk_bg: Color32,
    pub muted: Color32,
    pub text: Color32,
    pub hover: Color32,
}

impl DiffColors {
    pub fn from_theme(c: &UiColors) -> Self {
        Self {
            added_bg: c.added.0.gamma_multiply(0.16),
            removed_bg: c.deleted.0.gamma_multiply(0.16),
            added_fg: c.added.0,
            removed_fg: c.deleted.0,
            modified_fg: c.modified.0,
            hunk_bg: c.accent.0.gamma_multiply(0.10),
            muted: c.text_muted.0,
            text: c.text.0,
            hover: c.widget_hover.0.gamma_multiply(0.6),
        }
    }
}

pub fn status_letter(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Modified => "M",
        FileStatus::Added => "A",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
        FileStatus::Copied => "C",
    }
}

pub fn status_color(status: FileStatus, colors: &DiffColors) -> Color32 {
    match status {
        FileStatus::Added => colors.added_fg,
        FileStatus::Deleted => colors.removed_fg,
        _ => colors.modified_fg,
    }
}

/// Files with more changed lines than this start collapsed.
const LARGE_FILE_LINES: usize = 1500;

/// Renders all `files`. `id_salt` keeps collapse state separate per view.
/// When `scroll_to` names a file index, that file is scrolled into view.
pub fn show(
    ui: &mut Ui,
    id_salt: egui::Id,
    files: &[FileDiff],
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
    scroll_to: Option<usize>,
) {
    if files.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new("No changes.").weak());
        return;
    }
    for (idx, file) in files.iter().enumerate() {
        let id = id_salt.with(("file", idx, file.path()));
        let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
        let state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            id,
            lines <= LARGE_FILE_LINES,
        );
        let header = egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .corner_radius(4)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                state
                    .show_header(ui, |ui| file_header(ui, idx, file, colors, hooks))
                    .body(|ui| file_body(ui, idx, file, colors, hooks))
            });
        if scroll_to == Some(idx) {
            header.response.scroll_to_me(Some(egui::Align::TOP));
        }
        ui.add_space(6.0);
    }
}

fn file_header(
    ui: &mut Ui,
    idx: usize,
    file: &FileDiff,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
) {
    ui.label(
        RichText::new(status_letter(file.status))
            .monospace()
            .strong()
            .color(status_color(file.status, colors)),
    );
    if file.status == FileStatus::Renamed
        && let Some(old) = &file.old_path
    {
        ui.label(RichText::new(format!("{old} →")).color(colors.muted));
    }
    ui.label(RichText::new(file.path()).strong());
    ui.label(
        RichText::new(format!("+{}", file.additions()))
            .color(colors.added_fg)
            .small(),
    );
    ui.label(
        RichText::new(format!("−{}", file.deletions()))
            .color(colors.removed_fg)
            .small(),
    );
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        hooks.file_actions(ui, idx, file);
    });
}

fn file_body(
    ui: &mut Ui,
    idx: usize,
    file: &FileDiff,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
) {
    hooks.file_preamble(ui, file);
    if file.binary {
        ui.label(RichText::new("Binary file not shown.").weak());
        return;
    }
    if file.hunks.is_empty() {
        let msg = match file.status {
            FileStatus::Renamed => "File renamed without changes.",
            _ => "No content changes (mode change only).",
        };
        ui.label(RichText::new(msg).weak());
        return;
    }
    let font = TextStyle::Monospace.resolve(ui.style());
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 3.0;
    let max_no = file
        .hunks
        .iter()
        .map(|h| (h.old_start + h.old_count).max(h.new_start + h.new_count))
        .max()
        .unwrap_or(1);
    let digits = max_no.max(1).ilog10() as f32 + 1.0;
    let num_w = digits.max(3.0) * char_w + 10.0;
    let longest = file
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l| display_len(&l.text))
        .max()
        .unwrap_or(0);
    let content_w = num_w * 2.0 + 18.0 + longest as f32 * char_w + 16.0;

    ui.spacing_mut().item_spacing.y = 0.0;
    for (h_idx, hunk) in file.hunks.iter().enumerate() {
        hunk_header(ui, idx, h_idx, file, hunk, colors, hooks, row_h);
        for line in &hunk.lines {
            let geometry = RowGeometry {
                num_w,
                row_h,
                width: content_w.max(ui.available_width()),
            };
            diff_row(ui, file, line, &geometry, &font, colors, hooks);
            hooks.after_line(ui, file, line);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn hunk_header(
    ui: &mut Ui,
    file_idx: usize,
    hunk_idx: usize,
    file: &FileDiff,
    hunk: &Hunk,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
    row_h: f32,
) {
    egui::Frame::new()
        .fill(colors.hunk_bg)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(row_h);
            ui.horizontal(|ui| {
                let ranges = format!(
                    "@@ -{},{} +{},{} @@",
                    hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
                );
                ui.label(RichText::new(ranges).monospace().color(colors.muted));
                // Actions sit right after the ranges so they stay visible
                // even when long lines make the diff scroll horizontally.
                ui.spacing_mut().item_spacing.x = 4.0;
                hooks.hunk_actions(ui, file_idx, hunk_idx, file);
                if !hunk.section().is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(hunk.section())
                            .monospace()
                            .color(colors.muted),
                    );
                }
            });
        });
}

struct RowGeometry {
    num_w: f32,
    row_h: f32,
    width: f32,
}

fn diff_row(
    ui: &mut Ui,
    file: &FileDiff,
    line: &DiffLine,
    g: &RowGeometry,
    font: &FontId,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
) {
    let clickable = hooks.lines_clickable() && line_anchor(line).is_some();
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(g.width, g.row_h), sense);
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let (bg, sign, sign_color) = match line.kind {
        LineKind::Added => (colors.added_bg, "+", colors.added_fg),
        LineKind::Removed => (colors.removed_bg, "−", colors.removed_fg),
        LineKind::Context => (Color32::TRANSPARENT, " ", colors.muted),
    };
    painter.rect_filled(rect, 0, bg);
    if clickable && response.hovered() {
        painter.rect_filled(rect, 0, colors.hover);
    }
    let y = rect.top() + 1.5;
    let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
    painter.text(
        pos2(rect.left() + g.num_w - 6.0, y),
        Align2::RIGHT_TOP,
        num(line.old_no),
        font.clone(),
        colors.muted,
    );
    painter.text(
        pos2(rect.left() + g.num_w * 2.0 - 6.0, y),
        Align2::RIGHT_TOP,
        num(line.new_no),
        font.clone(),
        colors.muted,
    );
    let text_x = rect.left() + g.num_w * 2.0 + 4.0;
    painter.text(
        pos2(text_x, y),
        Align2::LEFT_TOP,
        sign,
        font.clone(),
        sign_color,
    );
    painter.text(
        pos2(text_x + 14.0, y),
        Align2::LEFT_TOP,
        display_text(&line.text),
        font.clone(),
        colors.text,
    );
    if line.no_newline {
        painter.text(
            pos2(rect.right() - 6.0, y),
            Align2::RIGHT_TOP,
            "no newline at end of file",
            FontId::proportional(10.0),
            colors.muted,
        );
    }
    if clickable {
        if response.hovered() {
            // "+" marker in the gutter, as on GitHub.
            let c = pos2(rect.left() + g.num_w * 2.0 - 2.0, rect.center().y);
            let r = Rect::from_center_size(c, vec2(14.0, 14.0));
            painter.rect_filled(r, 3, colors.added_fg.gamma_multiply(0.9));
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "+",
                FontId::monospace(11.0),
                Color32::BLACK,
            );
        }
        let response = response
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Comment on this line");
        if response.clicked() {
            hooks.line_clicked(file, line);
        }
    }
}

fn display_text(text: &str) -> String {
    text.trim_end_matches('\r').replace('\t', "    ")
}

fn display_len(text: &str) -> usize {
    text.trim_end_matches('\r')
        .chars()
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

//! Renders parsed unified diffs: one collapsible section per file, hunks
//! with old/new line numbers, syntax highlighting, word-level emphasis of
//! what changed, and a unified or side-by-side (split) layout.
//!
//! Callers customize it through [`DiffHooks`]: buttons on file and hunk
//! headers (stage hunk, open file, …), clickable lines (selecting lines to
//! stage, starting a review comment) and content under individual lines
//! (inline review comments).

use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CursorIcon, FontId, Painter, Pos2, Rect, RichText, Sense, TextStyle, Ui, pos2,
    vec2,
};

use super::diff_style::{self, LineStyle};
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

/// Position of a line within the diff being shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineRef {
    pub file: usize,
    pub hunk: usize,
    pub line: usize,
}

pub trait DiffHooks {
    /// Extra widgets on a file header (right-aligned).
    fn file_actions(&mut self, _ui: &mut Ui, _file_idx: usize, _file: &FileDiff) {}
    /// Extra widgets on a hunk header, after the line ranges.
    fn hunk_actions(&mut self, _ui: &mut Ui, _file_idx: usize, _hunk_idx: usize, _file: &FileDiff) {
    }
    /// Whether clicking `line` does something.
    fn can_click(&self, _line: &DiffLine) -> bool {
        false
    }
    /// Tooltip for clickable lines.
    fn click_hint(&self) -> &'static str {
        "Comment on this line"
    }
    fn line_clicked(&mut self, _at: LineRef, _file: &FileDiff, _line: &DiffLine, _shift: bool) {}
    /// Whether `at` is selected (drawn highlighted).
    fn is_selected(&self, _at: LineRef) -> bool {
        false
    }
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
    pub added_emph: Color32,
    pub removed_emph: Color32,
    pub added_fg: Color32,
    pub removed_fg: Color32,
    pub modified_fg: Color32,
    pub hunk_bg: Color32,
    pub muted: Color32,
    pub text: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub empty: Color32,
}

impl DiffColors {
    pub fn from_theme(c: &UiColors) -> Self {
        Self {
            added_bg: c.added.0.gamma_multiply(0.14),
            removed_bg: c.deleted.0.gamma_multiply(0.14),
            added_emph: c.added.0.gamma_multiply(0.38),
            removed_emph: c.deleted.0.gamma_multiply(0.38),
            added_fg: c.added.0,
            removed_fg: c.deleted.0,
            modified_fg: c.modified.0,
            hunk_bg: c.accent.0.gamma_multiply(0.10),
            muted: c.text_muted.0,
            text: c.text.0,
            hover: c.widget_hover.0.gamma_multiply(0.6),
            selected: c.accent.0.gamma_multiply(0.30),
            empty: c.border.0.gamma_multiply(0.25),
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

fn split_id() -> egui::Id {
    egui::Id::unique("diff_split_view")
}

/// Whether diffs are shown side by side (shared by every diff view).
pub fn is_split(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| *d.get_persisted_mut_or_default::<bool>(split_id()))
}

/// Unified / Split switch for a diff view's toolbar.
pub fn layout_toggle(ui: &mut Ui) {
    let mut split = is_split(ui.ctx());
    let before = split;
    ui.selectable_value(&mut split, true, "Split")
        .on_hover_text("Show old and new side by side");
    ui.selectable_value(&mut split, false, "Unified")
        .on_hover_text("Show removed and added lines interleaved");
    if split != before {
        ui.ctx().data_mut(|d| d.insert_persisted(split_id(), split));
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
    let split = is_split(ui.ctx());
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
                    .body(|ui| file_body(ui, id, idx, file, colors, hooks, split))
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

type Styles = Arc<Vec<Vec<LineStyle>>>;

/// Style for lines whose highlighting hasn't been computed.
static NO_STYLE: LineStyle = LineStyle {
    spans: Vec::new(),
    emph: Vec::new(),
};

/// Syntax and word styles for `file`, cached until its content or the
/// theme changes.
fn file_styles(ctx: &egui::Context, id: egui::Id, file: &FileDiff) -> Styles {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    diff_style::generation().hash(&mut h);
    file.path().hash(&mut h);
    for hunk in &file.hunks {
        hunk.header.hash(&mut h);
        for l in &hunk.lines {
            l.text.hash(&mut h);
        }
    }
    let key = h.finish();
    let cache_id = id.with("styles");
    if let Some((k, styles)) = ctx.data(|d| d.get_temp::<(u64, Styles)>(cache_id))
        && k == key
    {
        return styles;
    }
    let styles = Arc::new(diff_style::style_file(file));
    ctx.data_mut(|d| d.insert_temp(cache_id, (key, styles.clone())));
    styles
}

struct Geometry {
    num_w: f32,
    row_h: f32,
    char_w: f32,
    font: FontId,
}

#[allow(clippy::too_many_arguments)]
fn file_body(
    ui: &mut Ui,
    id: egui::Id,
    idx: usize,
    file: &FileDiff,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
    split: bool,
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
    // Highlighting is the expensive part: only do it for files on screen
    // (rows outside the viewport aren't painted anyway).
    let rows: usize = file.hunks.iter().map(|h| h.lines.len() + 1).sum();
    let estimate = Rect::from_min_size(
        ui.cursor().min,
        vec2(ui.available_width(), rows as f32 * row_h),
    );
    let styles = ui
        .is_rect_visible(estimate)
        .then(|| file_styles(ui.ctx(), id, file));
    let max_no = file
        .hunks
        .iter()
        .map(|h| (h.old_start + h.old_count).max(h.new_start + h.new_count))
        .max()
        .unwrap_or(1);
    let digits = max_no.max(1).ilog10() as f32 + 1.0;
    let g = Geometry {
        num_w: digits.max(3.0) * char_w + 10.0,
        row_h,
        char_w,
        font,
    };
    let longest = file
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .map(|l| display_len(&l.text))
        .max()
        .unwrap_or(0);

    ui.spacing_mut().item_spacing.y = 0.0;
    for (h_idx, hunk) in file.hunks.iter().enumerate() {
        hunk_header(ui, idx, h_idx, file, hunk, colors, hooks, row_h);
        let st: &[LineStyle] = styles.as_ref().map_or(&[], |s| &s[h_idx]);
        if split {
            split_rows(ui, idx, h_idx, file, hunk, st, &g, colors, hooks);
        } else {
            let width =
                (g.num_w * 2.0 + 18.0 + longest as f32 * char_w + 16.0).max(ui.available_width());
            for (i, line) in hunk.lines.iter().enumerate() {
                let at = LineRef {
                    file: idx,
                    hunk: h_idx,
                    line: i,
                };
                let style = st.get(i).unwrap_or(&NO_STYLE);
                unified_row(ui, at, file, line, style, width, &g, colors, hooks);
                hooks.after_line(ui, file, line);
            }
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

/// Background, sign, sign color and word-emphasis color for a line kind.
fn kind_colors(kind: LineKind, colors: &DiffColors) -> (Color32, &'static str, Color32, Color32) {
    match kind {
        LineKind::Added => (colors.added_bg, "+", colors.added_fg, colors.added_emph),
        LineKind::Removed => (
            colors.removed_bg,
            "−",
            colors.removed_fg,
            colors.removed_emph,
        ),
        LineKind::Context => (
            Color32::TRANSPARENT,
            " ",
            colors.muted,
            Color32::TRANSPARENT,
        ),
    }
}

/// Selection highlight, hover marker and clicks for one line's area.
#[allow(clippy::too_many_arguments)]
fn row_interaction(
    ui: &Ui,
    painter: &Painter,
    response: &egui::Response,
    area: Rect,
    at: LineRef,
    file: &FileDiff,
    line: &DiffLine,
    gutter_x: f32,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
) {
    if hooks.is_selected(at) {
        painter.rect_filled(area, 0, colors.selected);
    }
    if !hooks.can_click(line) {
        return;
    }
    let hovered = response.hovered() && ui.rect_contains_pointer(area);
    if hovered {
        painter.rect_filled(area, 0, colors.hover);
        // "+" marker in the gutter, as on GitHub.
        let c = pos2(gutter_x, area.center().y);
        let r = Rect::from_center_size(c, vec2(14.0, 14.0));
        painter.rect_filled(r, 3, colors.added_fg.gamma_multiply(0.9));
        painter.text(
            c,
            Align2::CENTER_CENTER,
            "+",
            FontId::monospace(11.0),
            Color32::BLACK,
        );
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if hovered && response.clicked() {
        let shift = ui.input(|i| i.modifiers.shift);
        hooks.line_clicked(at, file, line, shift);
    }
}

#[allow(clippy::too_many_arguments)]
fn unified_row(
    ui: &mut Ui,
    at: LineRef,
    file: &FileDiff,
    line: &DiffLine,
    style: &LineStyle,
    width: f32,
    g: &Geometry,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, g.row_h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter().clone();
    let (bg, sign, sign_color, emph) = kind_colors(line.kind, colors);
    painter.rect_filled(rect, 0, bg);
    let gutter_x = rect.left() + g.num_w * 2.0 - 2.0;
    row_interaction(
        ui, &painter, &response, rect, at, file, line, gutter_x, colors, hooks,
    );
    let y = rect.top() + 1.5;
    let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
    painter.text(
        pos2(rect.left() + g.num_w - 6.0, y),
        Align2::RIGHT_TOP,
        num(line.old_no),
        g.font.clone(),
        colors.muted,
    );
    painter.text(
        pos2(rect.left() + g.num_w * 2.0 - 6.0, y),
        Align2::RIGHT_TOP,
        num(line.new_no),
        g.font.clone(),
        colors.muted,
    );
    let text_x = rect.left() + g.num_w * 2.0 + 4.0;
    painter.text(
        pos2(text_x, y),
        Align2::LEFT_TOP,
        sign,
        g.font.clone(),
        sign_color,
    );
    paint_code(
        &painter,
        pos2(text_x + 14.0, y),
        &line.text,
        style,
        &g.font,
        colors.text,
        emph,
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
    if hooks.can_click(line) {
        response.on_hover_text(hooks.click_hint());
    }
}

/// Side-by-side rows: context on both sides, removed lines on the left
/// paired with the added lines that replace them on the right.
#[allow(clippy::too_many_arguments)]
fn split_rows(
    ui: &mut Ui,
    file_idx: usize,
    hunk_idx: usize,
    file: &FileDiff,
    hunk: &Hunk,
    styles: &[LineStyle],
    g: &Geometry,
    colors: &DiffColors,
    hooks: &mut dyn DiffHooks,
) {
    let kinds: Vec<LineKind> = hunk.lines.iter().map(|l| l.kind).collect();
    let runs = diff_style::change_runs(&kinds);
    let mut pairs: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    let mut run_iter = runs.iter();
    let mut i = 0;
    while i < kinds.len() {
        if kinds[i] == LineKind::Context {
            pairs.push((Some(i), Some(i)));
            i += 1;
            continue;
        }
        let Some((removed, added)) = run_iter.next() else {
            break;
        };
        for k in 0..removed.len().max(added.len()) {
            pairs.push((removed.get(k).copied(), added.get(k).copied()));
        }
        i += removed.len() + added.len();
    }

    let width = ui.available_width();
    let half = (width / 2.0).floor();
    for (left, right) in pairs {
        let (rect, response) = ui.allocate_exact_size(vec2(width, g.row_h), Sense::click());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter().clone();
            let halves = [
                (
                    Rect::from_min_size(rect.min, vec2(half, rect.height())),
                    left,
                    true,
                ),
                (
                    Rect::from_min_max(pos2(rect.left() + half, rect.top()), rect.max),
                    right,
                    false,
                ),
            ];
            for (area, idx, is_left) in halves {
                let p = painter.with_clip_rect(area.intersect(ui.clip_rect()));
                let Some(i) = idx else {
                    p.rect_filled(area, 0, colors.empty);
                    continue;
                };
                let line = &hunk.lines[i];
                let (bg, sign, sign_color, emph) = kind_colors(line.kind, colors);
                p.rect_filled(area, 0, bg);
                let at = LineRef {
                    file: file_idx,
                    hunk: hunk_idx,
                    line: i,
                };
                let gutter_x = area.left() + g.num_w - 2.0;
                row_interaction(
                    ui, &p, &response, area, at, file, line, gutter_x, colors, hooks,
                );
                let y = area.top() + 1.5;
                let no = if is_left { line.old_no } else { line.new_no };
                p.text(
                    pos2(area.left() + g.num_w - 6.0, y),
                    Align2::RIGHT_TOP,
                    no.map(|n| n.to_string()).unwrap_or_default(),
                    g.font.clone(),
                    colors.muted,
                );
                let text_x = area.left() + g.num_w + 4.0;
                p.text(
                    pos2(text_x, y),
                    Align2::LEFT_TOP,
                    sign,
                    g.font.clone(),
                    sign_color,
                );
                paint_code(
                    &p,
                    pos2(text_x + g.char_w + 6.0, y),
                    &line.text,
                    styles.get(i).unwrap_or(&NO_STYLE),
                    &g.font,
                    colors.text,
                    emph,
                );
            }
            painter.vline(
                rect.left() + half,
                rect.y_range(),
                egui::Stroke::new(1.0, colors.empty.gamma_multiply(2.0)),
            );
            let clickable = [left, right]
                .into_iter()
                .flatten()
                .any(|i| hooks.can_click(&hunk.lines[i]));
            if clickable {
                response.on_hover_text(hooks.click_hint());
            }
        }
        // Threads under the row: the removed line's, then the right side's.
        if let Some(l) = left
            && hunk.lines[l].kind == LineKind::Removed
        {
            hooks.after_line(ui, file, &hunk.lines[l]);
        }
        if let Some(r) = right {
            hooks.after_line(ui, file, &hunk.lines[r]);
        }
    }
}

/// Paints one line of code with syntax colors and word emphasis.
fn paint_code(
    painter: &Painter,
    pos: Pos2,
    text: &str,
    style: &LineStyle,
    font: &FontId,
    default: Color32,
    emph_bg: Color32,
) {
    let text = text.trim_end_matches('\r');
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for (r, _) in &style.spans {
        cuts.extend([r.start, r.end]);
    }
    for r in &style.emph {
        cuts.extend([r.start, r.end]);
    }
    cuts.retain(|&c| c <= text.len() && text.is_char_boundary(c));
    cuts.sort_unstable();
    cuts.dedup();
    let mut job = LayoutJob::default();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let color = style
            .spans
            .iter()
            .find(|(r, _)| r.start <= a && a < r.end)
            .map_or(default, |(_, c)| *c);
        let mut format = TextFormat::simple(font.clone(), color);
        if style.emph.iter().any(|r| r.start <= a && a < r.end) {
            format.background = emph_bg;
        }
        job.append(&text[a..b].replace('\t', "    "), 0.0, format);
    }
    let galley = painter.layout_job(job);
    painter.galley(pos, galley, default);
}

fn display_len(text: &str) -> usize {
    text.trim_end_matches('\r')
        .chars()
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

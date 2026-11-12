//! The minimap: a zoomed-out view of the whole file beside the editor, with
//! the visible part outlined. Click or drag it to scroll; the mouse wheel
//! over it scrolls the editor.
//!
//! Each row of the editor becomes a 2-pixel strip, and each run of non-blank
//! characters a block in that run's syntax color. Only the strips in view
//! are built, from the buffer and its highlighting, so the minimap costs the
//! same for any file size.

use egui::{Color32, Mesh, Rect, Sense, Shape, pos2, vec2};
use syntect::highlighting::Style;

/// Width of the minimap, in points.
pub const WIDTH: f32 = 96.0;
/// Height of one row of text in the minimap.
const ROW_HEIGHT: f32 = 2.0;
/// Width of one character column in the minimap.
const COLUMN_WIDTH: f32 = 1.0;
/// Left padding inside the minimap.
const PADDING: f32 = 6.0;

/// One block of characters: start and end column, and color.
pub type Run = (f32, f32, Color32);

/// Where the editor is scrolled, from its scroll area.
pub struct Viewport {
    /// Vertical scroll offset.
    pub offset: f32,
    /// Height of the visible part.
    pub height: f32,
    /// Height of everything that can be scrolled through.
    pub content_height: f32,
    /// Height of one editor row.
    pub row_height: f32,
    /// Editor rows (display rows: folded lines hidden, wrapped lines split).
    pub rows: usize,
}

/// Draws the minimap in `rect`. `runs` gives the blocks of an editor row.
/// Returns the editor scroll offset to move to, when the user clicked,
/// dragged or scrolled it.
pub fn show(
    ui: &mut egui::Ui,
    rect: Rect,
    view: &Viewport,
    mut runs: impl FnMut(usize) -> Vec<Run>,
) -> Option<f32> {
    let row_count = view.rows.max(1);
    let row_height = view.row_height;
    if !row_height.is_finite() || row_height <= 0.0 {
        return None;
    }
    let max_offset = (view.content_height - view.height).max(0.0);
    let map_height = row_count as f32 * ROW_HEIGHT;
    // Rows to editor points, in minimap points.
    let scale = ROW_HEIGHT / row_height;
    let slider_height = (view.height * scale).max(ROW_HEIGHT * 2.0);
    // A long file doesn't fit: the minimap scrolls along with the editor.
    let map_scroll = if map_height > rect.height() && max_offset > 0.0 {
        (view.offset / max_offset).clamp(0.0, 1.0) * (map_height - rect.height())
    } else {
        0.0
    };

    let id = ui.scope_id().with("minimap");
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 0.0, visuals.text_edit_bg_color());
    painter.vline(
        rect.left(),
        rect.y_range(),
        egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );

    // Text blocks, only for the rows in view.
    let first = (map_scroll / ROW_HEIGHT) as usize;
    let last = (first + (rect.height() / ROW_HEIGHT).ceil() as usize + 1).min(view.rows);
    let max_x = rect.right() - 2.0;
    let mut mesh = Mesh::default();
    for i in first..last {
        let y = rect.top() + i as f32 * ROW_HEIGHT - map_scroll;
        for (a, b, color) in runs(i) {
            let x0 = rect.left() + PADDING + a * COLUMN_WIDTH;
            if x0 >= max_x {
                break;
            }
            let x1 = (rect.left() + PADDING + b * COLUMN_WIDTH).min(max_x);
            let block = Rect::from_min_max(pos2(x0, y), pos2(x1, y + ROW_HEIGHT * 0.8));
            mesh.add_colored_rect(block, color);
        }
    }
    painter.add(Shape::mesh(mesh));

    // The visible part of the editor.
    let slider_top = rect.top() + view.offset * scale - map_scroll;
    let slider = Rect::from_min_size(
        pos2(rect.left() + 1.0, slider_top),
        vec2(rect.width() - 1.0, slider_height),
    );
    let active = response.hovered() || response.dragged();
    let fill = visuals
        .widgets
        .inactive
        .bg_fill
        .gamma_multiply(if active { 0.55 } else { 0.3 });
    painter.rect_filled(slider, 0.0, fill);

    // Clicking away from the slider centers the editor there; dragging
    // moves the slider with the pointer.
    let mut target = None;
    if response.drag_started() || response.clicked() {
        if let Some(p) = response.interact_pointer_pos()
            && !slider.contains(p)
        {
            let row = (p.y - rect.top() + map_scroll) / ROW_HEIGHT;
            target = Some(row * row_height - view.height / 2.0);
        }
    } else if response.dragged() {
        let track = map_height.min(rect.height()) - slider_height;
        let ratio = if track > 0.0 {
            max_offset / track
        } else {
            1.0 / scale
        };
        target = Some(view.offset + response.drag_delta().y * ratio);
    }
    if response.hovered() {
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            target = Some(target.unwrap_or(view.offset) - wheel);
        }
    }
    target
        .map(|t| t.clamp(0.0, max_offset))
        .filter(|t| (t - view.offset).abs() > 0.5)
}

/// The blocks for the chars `range` of a line whose syntax spans are
/// `spans` (byte lengths, as from the highlight cache), in columns from the
/// start of the range. Without spans the text is drawn in `fg`.
pub fn line_runs(
    text: &str,
    range: std::ops::Range<usize>,
    spans: Option<&[(Style, usize)]>,
    fg: Color32,
) -> Vec<Run> {
    let max_columns = (WIDTH - PADDING) / COLUMN_WIDTH;
    let mut runs: Vec<Run> = Vec::new();
    let mut span = 0;
    let mut span_end = spans.and_then(|s| s.first()).map_or(usize::MAX, |s| s.1);
    let mut col = 0.0;
    for (i, (byte, c)) in text.char_indices().enumerate() {
        while byte >= span_end {
            span += 1;
            span_end += spans.and_then(|s| s.get(span)).map_or(usize::MAX, |s| s.1);
        }
        if i < range.start {
            continue;
        }
        if i >= range.end || col >= max_columns {
            break;
        }
        let width = if c == '\t' { 4.0 } else { 1.0 };
        if !c.is_whitespace() {
            let color = match spans.and_then(|s| s.get(span)) {
                Some((style, _)) => {
                    let f = style.foreground;
                    Color32::from_rgb(f.r, f.g, f.b)
                }
                None => fg,
            }
            .gamma_multiply(0.8);
            match runs.last_mut() {
                // Extend the run when the color matches and nothing (no
                // space) lies between.
                Some(last) if last.2 == color && col - last.1 < 0.5 => last.1 = col + width,
                _ => runs.push((col, col + width, color)),
            }
        }
        col += width;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_follow_words_and_colors() {
        let red = Style {
            foreground: syntect::highlighting::Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
            ..Style::default()
        };
        let blue = Style {
            foreground: syntect::highlighting::Color {
                r: 0,
                g: 0,
                b: 255,
                a: 255,
            },
            ..Style::default()
        };
        let spans = [(red, 4), (blue, 6)];
        let runs = line_runs("let x\tyz", 0..8, Some(&spans), Color32::WHITE);
        let cols: Vec<(f32, f32)> = runs.iter().map(|r| (r.0, r.1)).collect();
        // "let" in red, "x" and "yz" in blue (after a 4-column tab).
        assert_eq!(cols, vec![(0.0, 3.0), (4.0, 5.0), (9.0, 11.0)]);
        assert_ne!(runs[0].2, runs[1].2);
        // A wrapped row starts counting columns at its first char.
        let runs = line_runs("abc def", 4..7, None, Color32::WHITE);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].0, runs[0].1), (0.0, 3.0));
    }
}

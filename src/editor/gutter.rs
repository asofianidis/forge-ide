//! Line-number gutter painted to the left of the editor text: line numbers,
//! fold markers and source control change markers.

use egui::{Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, pos2, vec2};

use super::widget::Layout;
use crate::git::line_changes::{LineChanges, Mark};

/// Colors for the source control change markers.
pub struct MarkColors {
    pub added: Color32,
    pub modified: Color32,
    pub deleted: Color32,
}

/// Horizontal padding on each side of the line numbers, in points.
const PADDING: f32 = 8.0;

/// Width of the fold marker column between the numbers and the change markers.
const FOLD_WIDTH: f32 = 14.0;

/// The gutter is never wider than this.
const MAX_WIDTH: f32 = 200.0;

/// Width of a gutter that fits `line_count` line numbers, or just the fold
/// and change markers when line numbers are hidden.
pub fn width(line_count: usize, digit_width: f32, numbers: bool) -> f32 {
    if !numbers {
        return 2.0 * PADDING + FOLD_WIDTH;
    }
    let digits = line_count.max(1).ilog10() as f32 + 1.0;
    (digits.max(2.0) * digit_width + 2.0 * PADDING + FOLD_WIDTH).min(MAX_WIDTH)
}

/// The x of the line between the gutter and the text.
fn separator_x(gutter: Rect) -> f32 {
    gutter.right() - PADDING / 2.0
}

/// Horizontal range of the fold markers.
fn fold_x(gutter: Rect) -> egui::Rangef {
    let right = separator_x(gutter) - 6.0;
    egui::Rangef::new(right - FOLD_WIDTH, right)
}

/// Paints line numbers (if `numbers`) next to each visible line.
///
/// Wrapped continuation rows get no number. When `changes` is given, lines
/// that differ from the committed version get a colored bar (added/modified)
/// or a small wedge (deleted lines above).
pub fn paint(
    ui: &egui::Ui,
    layout: &Layout,
    current_line: usize,
    font: &FontId,
    numbers: bool,
    changes: Option<(&LineChanges, &MarkColors)>,
) {
    let painter = ui.painter().with_clip_rect(layout.gutter);
    let visuals = ui.visuals();
    let dim = visuals.weak_text_color();
    let strong = visuals.strong_text_color();
    let gutter = layout.gutter;
    let separator = separator_x(gutter);
    let right = fold_x(gutter).min - 2.0;
    painter.line_segment(
        [
            pos2(separator, gutter.top()),
            pos2(separator, gutter.bottom()),
        ],
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );
    for (line, starts_line, rect) in rows(layout) {
        if let Some((changes, colors)) = changes {
            paint_mark(
                &painter,
                changes.get(line),
                starts_line,
                rect,
                separator,
                colors,
            );
        }
        if numbers && starts_line {
            let color: Color32 = if line == current_line { strong } else { dim };
            painter.text(
                pos2(right, rect.top()),
                Align2::RIGHT_TOP,
                line.to_string(),
                font.clone(),
                color,
            );
        }
    }
}

/// Screen rects of the visible rows, with the 1-based line each belongs to
/// and whether it is the first row of that line.
pub fn rows(layout: &Layout) -> impl Iterator<Item = (usize, bool, Rect)> + '_ {
    layout.row_rects().map(|(line, first, rect)| {
        let rect =
            Rect::from_x_y_ranges(rect.x_range(), rect.top()..=rect.top() + layout.row_height);
        (line + 1, first, rect)
    })
}

/// Screen rect of all rows of 1-based `line`, full editor width.
pub fn line_rect(layout: &Layout, line: usize) -> Option<Rect> {
    layout.line_rect(line.checked_sub(1)?)
}

/// The 1-based line whose change marker is at `pos`, if `pos` is on the marker strip.
pub fn marker_hit(layout: &Layout, pos: Pos2) -> Option<usize> {
    let separator = separator_x(layout.gutter);
    if !(separator - 7.0..=separator + 2.0).contains(&pos.x) {
        return None;
    }
    rows(layout)
        .find(|(_, _, r)| r.y_range().contains(pos.y))
        .map(|(l, _, _)| l)
}

/// Whether `pos` is in the fold marker column.
pub fn fold_marker_hit(gutter: Rect, pos: Pos2) -> bool {
    fold_x(gutter).expand(1.0).contains(pos.x)
}

/// A chevron in the fold column: pointing right when folded, down otherwise.
pub fn paint_fold_marker(
    painter: &egui::Painter,
    gutter: Rect,
    row: Rect,
    folded: bool,
    color: Color32,
) {
    let x = fold_x(gutter);
    let c = pos2(x.center(), row.center().y);
    let s = 3.5;
    let points = if folded {
        vec![
            c + vec2(-s / 2.0, -s),
            c + vec2(s / 2.0, 0.0),
            c + vec2(-s / 2.0, s),
        ]
    } else {
        vec![
            c + vec2(-s, -s / 2.0),
            c + vec2(0.0, s / 2.0),
            c + vec2(s, -s / 2.0),
        ]
    };
    painter.add(Shape::line(points, Stroke::new(1.5, color)));
}

fn paint_mark(
    painter: &egui::Painter,
    mark: Option<Mark>,
    starts_line: bool,
    row: Rect,
    separator_x: f32,
    colors: &MarkColors,
) {
    let bar = |color: Color32| {
        let r = Rect::from_x_y_ranges(separator_x - 4.0..=separator_x - 1.0, row.y_range());
        painter.rect_filled(r, 0, color);
    };
    match mark {
        Some(Mark::Added) => bar(colors.added),
        Some(Mark::Modified) => bar(colors.modified),
        Some(Mark::DeletedAbove) if starts_line => {
            let tip = pos2(separator_x - 1.0, row.top());
            painter.add(Shape::convex_polygon(
                vec![tip - vec2(5.0, 4.0), tip, tip - vec2(5.0, -4.0)],
                colors.deleted,
                Stroke::NONE,
            ));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_grows_with_digit_count() {
        assert_eq!(width(1, 10.0, true), width(99, 10.0, true));
        assert!(width(100, 10.0, true) > width(99, 10.0, true));
        assert!(width(10_000, 10.0, true) > width(1_000, 10.0, true));
    }

    #[test]
    fn width_is_capped_to_margin_range() {
        assert_eq!(width(usize::MAX, 50.0, true), MAX_WIDTH);
    }

    #[test]
    fn hidden_numbers_leave_room_for_change_markers() {
        assert_eq!(width(1, 10.0, false), width(100_000, 10.0, false));
        assert!(width(1, 10.0, false) < width(1, 10.0, true));
    }
}

//! Line-number gutter painted into the left margin of the editor `TextEdit`.

use egui::text_edit::TextEditOutput;
use egui::{Align2, Color32, FontId, Rect, Shape, Stroke, pos2, vec2};

use crate::git::line_changes::{LineChanges, Mark};

/// Colors for the source control change markers.
pub struct MarkColors {
    pub added: Color32,
    pub modified: Color32,
    pub deleted: Color32,
}

/// Horizontal padding on each side of the line numbers, in points.
const PADDING: f32 = 8.0;

/// `TextEdit` margins are `i8`, so the gutter can never be wider than this.
const MAX_WIDTH: f32 = i8::MAX as f32;

/// Width of a gutter that fits `line_count` line numbers, or just the change
/// markers when line numbers are hidden.
pub fn width(line_count: usize, digit_width: f32, numbers: bool) -> f32 {
    if !numbers {
        return 2.0 * PADDING;
    }
    let digits = line_count.max(1).ilog10() as f32 + 1.0;
    (digits.max(2.0) * digit_width + 2.0 * PADDING).min(MAX_WIDTH)
}

/// Paints line numbers (if `numbers`) next to each logical line of the laid-out text.
///
/// Wrapped continuation rows get no number. Only rows inside the clip rect are painted.
/// When `changes` is given, lines that differ from the committed version get
/// a colored bar (added/modified) or a small wedge (deleted lines above).
pub fn paint(
    ui: &egui::Ui,
    output: &TextEditOutput,
    current_line: usize,
    font: &FontId,
    numbers: bool,
    changes: Option<(&LineChanges, &MarkColors)>,
) {
    let painter = ui.painter();
    let clip = ui.clip_rect();
    let visuals = ui.visuals();
    let dim = visuals.weak_text_color();
    let strong = visuals.strong_text_color();

    let right = output.galley_pos.x - PADDING;
    let separator_x = output.galley_pos.x - PADDING / 2.0;
    let frame = output.response.response.rect;
    painter.line_segment(
        [
            pos2(separator_x, frame.top()),
            pos2(separator_x, frame.bottom()),
        ],
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );

    let mut line = 1;
    let mut starts_line = true;
    for row in &output.galley.rows {
        let rect = row.rect().translate(output.galley_pos.to_vec2());
        let visible = rect.bottom() >= clip.top() && rect.top() <= clip.bottom();
        if visible && let Some((changes, colors)) = changes {
            paint_mark(
                painter,
                changes.get(line),
                starts_line,
                rect,
                separator_x,
                colors,
            );
        }
        if numbers && starts_line && visible {
            let color: Color32 = if line == current_line { strong } else { dim };
            painter.text(
                pos2(right, rect.top()),
                Align2::RIGHT_TOP,
                line.to_string(),
                font.clone(),
                color,
            );
        }
        if rect.top() > clip.bottom() {
            break;
        }
        starts_line = row.ends_with_newline;
        if starts_line {
            line += 1;
        }
    }
}

/// Screen rects of the laid-out rows, with the 1-based line each belongs to
/// and whether it is the first row of that line.
pub fn rows(output: &TextEditOutput) -> impl Iterator<Item = (usize, bool, Rect)> + '_ {
    let mut line = 1;
    let mut starts_line = true;
    output.galley.rows.iter().map(move |row| {
        let rect = row.rect().translate(output.galley_pos.to_vec2());
        let item = (line, starts_line, rect);
        starts_line = row.ends_with_newline;
        if starts_line {
            line += 1;
        }
        item
    })
}

/// Screen rect of all rows of 1-based `line`, full editor width.
pub fn line_rect(output: &TextEditOutput, line: usize) -> Option<Rect> {
    let frame = output.response.response.rect;
    rows(output)
        .filter(|(l, _, _)| *l == line)
        .map(|(_, _, r)| Rect::from_x_y_ranges(frame.x_range(), r.y_range()))
        .reduce(|a, b| a.union(b))
}

/// The line whose change marker is at `pos`, if `pos` is on the marker strip.
pub fn marker_hit(output: &TextEditOutput, pos: egui::Pos2) -> Option<usize> {
    let separator_x = output.galley_pos.x - PADDING / 2.0;
    if !(separator_x - 7.0..=separator_x + 2.0).contains(&pos.x) {
        return None;
    }
    rows(output)
        .find(|(_, _, r)| r.y_range().contains(pos.y))
        .map(|(l, _, _)| l)
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

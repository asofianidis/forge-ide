//! Line-number gutter painted into the left margin of the editor `TextEdit`.

use egui::text_edit::TextEditOutput;
use egui::{Align2, Color32, FontId, Stroke, pos2};

/// Horizontal padding on each side of the line numbers, in points.
const PADDING: f32 = 8.0;

/// `TextEdit` margins are `i8`, so the gutter can never be wider than this.
const MAX_WIDTH: f32 = i8::MAX as f32;

/// Width of a gutter that fits `line_count` line numbers.
pub fn width(line_count: usize, digit_width: f32) -> f32 {
    let digits = line_count.max(1).ilog10() as f32 + 1.0;
    (digits.max(2.0) * digit_width + 2.0 * PADDING).min(MAX_WIDTH)
}

/// Paints line numbers next to each logical line of the laid-out text.
///
/// Wrapped continuation rows get no number. Only rows inside the clip rect are painted.
pub fn paint(ui: &egui::Ui, output: &TextEditOutput, current_line: usize, font: &FontId) {
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
        if starts_line && rect.bottom() >= clip.top() && rect.top() <= clip.bottom() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_grows_with_digit_count() {
        assert_eq!(width(1, 10.0), width(99, 10.0));
        assert!(width(100, 10.0) > width(99, 10.0));
        assert!(width(10_000, 10.0) > width(1_000, 10.0));
    }

    #[test]
    fn width_is_capped_to_margin_range() {
        assert_eq!(width(usize::MAX, 50.0), MAX_WIDTH);
    }
}

//! The minimap: a zoomed-out view of the whole file beside the editor, with
//! the visible part outlined. Click or drag it to scroll; the mouse wheel
//! over it scrolls the editor.
//!
//! Each row of the laid-out text becomes a 2-pixel strip, and each run of
//! non-blank characters a block in that run's syntax color.

use std::sync::Arc;

use egui::{Color32, Galley, Mesh, Rect, Sense, Shape, pos2, vec2};

/// Width of the minimap, in points.
pub const WIDTH: f32 = 96.0;
/// Height of one row of text in the minimap.
const ROW_HEIGHT: f32 = 2.0;
/// Width of one character column in the minimap.
const COLUMN_WIDTH: f32 = 1.0;
/// Left padding inside the minimap.
const PADDING: f32 = 6.0;

/// One block of characters: start and end column, and color.
type Run = (f32, f32, Color32);

#[derive(Default)]
pub struct Minimap {
    /// Runs per galley row, for the galley they were built from. egui keeps
    /// reusing the same galley while the text and layout are unchanged.
    cache: Option<(Arc<Galley>, Vec<Vec<Run>>)>,
}

/// Where the editor is scrolled, from its scroll area.
pub struct Viewport {
    /// Vertical scroll offset.
    pub offset: f32,
    /// Height of the visible part.
    pub height: f32,
    /// Height of everything that can be scrolled through.
    pub content_height: f32,
}

impl Minimap {
    /// Draws the minimap for `galley` in `rect`. Returns the editor scroll
    /// offset to move to, when the user clicked, dragged or scrolled it.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        galley: &Arc<Galley>,
        char_width: f32,
        view: &Viewport,
    ) -> Option<f32> {
        let rows = self.rows(galley, char_width);
        let row_count = rows.len().max(1);
        // Height of a row in the editor.
        let row_height = galley.rect.height() / row_count as f32;
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
        let last = (first + (rect.height() / ROW_HEIGHT).ceil() as usize + 1).min(rows.len());
        let max_x = rect.right() - 2.0;
        let mut mesh = Mesh::default();
        for (i, runs) in rows.iter().enumerate().take(last).skip(first) {
            let y = rect.top() + i as f32 * ROW_HEIGHT - map_scroll;
            for &(a, b, color) in runs {
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

    fn rows(&mut self, galley: &Arc<Galley>, char_width: f32) -> &[Vec<Run>] {
        let stale = !matches!(&self.cache, Some((g, _)) if Arc::ptr_eq(g, galley));
        if stale {
            self.cache = Some((galley.clone(), build(galley, char_width)));
        }
        self.cache.as_ref().map_or(&[], |(_, rows)| rows)
    }
}

/// Runs of non-blank characters per row, in character columns.
fn build(galley: &Galley, char_width: f32) -> Vec<Vec<Run>> {
    let char_width = char_width.max(1.0);
    let max_columns = (WIDTH - PADDING) / COLUMN_WIDTH;
    galley
        .rows
        .iter()
        .map(|placed| {
            let row = &placed.row;
            let mut runs: Vec<Run> = Vec::new();
            for glyph in &row.glyphs {
                if glyph.chr.is_whitespace() {
                    continue;
                }
                let start = (placed.pos.x + glyph.pos.x) / char_width;
                if start >= max_columns {
                    break;
                }
                let end = start + (glyph.advance_width / char_width).max(1.0);
                let color = row
                    .visuals
                    .mesh
                    .vertices
                    .get(glyph.first_vertex as usize)
                    .map_or(Color32::GRAY, |v| v.color)
                    .gamma_multiply(0.8);
                match runs.last_mut() {
                    // Extend the run when the color matches and nothing
                    // (no space) lies between.
                    Some(last) if last.2 == color && start - last.1 < 0.5 => last.1 = end,
                    _ => runs.push((start, end, color)),
                }
            }
            runs
        })
        .collect()
}

//! A keyboard-driven list picker shown at the top of the window: a query
//! field above a filtered list. Quick open, the command palette and the theme
//! picker all use it.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, Key, Modifiers, TextStyle};

/// Rows shown at most; more matches than this are summarized.
pub const MAX_ROWS: usize = 200;

pub struct PickerItem {
    pub label: String,
    /// Secondary text shown on the right (a shortcut or folder).
    pub detail: String,
    /// Char indices in `label` to emphasize (the fuzzy-matched characters).
    pub highlights: Vec<usize>,
}

#[derive(Default)]
pub struct Picker {
    pub open: bool,
    pub query: String,
    selected: usize,
    /// Select the query text on the next frame (e.g. a prefilled query).
    select_query: bool,
}

/// Colors for the picker rows.
pub struct PickerStyle {
    pub highlight: Color32,
    pub muted: Color32,
}

impl Picker {
    pub fn open(&mut self, query: &str) {
        self.open = true;
        self.query = query.to_owned();
        self.selected = 0;
        self.select_query = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Draws the picker. Returns the index of the item the user chose (with
    /// Enter or a click), after which the picker is closed.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        hint: &str,
        items: &[PickerItem],
        footer: Option<&str>,
        style: &PickerStyle,
    ) -> Option<usize> {
        if !self.open {
            return None;
        }
        let shown = items.len().min(MAX_ROWS);
        let (down, up, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let moved = down || up;
        if shown > 0 {
            if down {
                self.selected = (self.selected + 1) % shown;
            }
            if up {
                self.selected = (self.selected + shown - 1) % shown;
            }
        }
        self.selected = self.selected.min(shown.saturating_sub(1));
        let mut chosen = (enter && shown > 0).then_some(self.selected);
        if escape {
            self.open = false;
            return None;
        }

        let id = egui::Id::unique("picker");
        let width = (ctx.content_rect().width() - 32.0).clamp(240.0, 640.0);
        let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, [0.0, 56.0]);
        let modal = egui::Modal::new(id)
            .area(area)
            .backdrop_color(Color32::from_black_alpha(60))
            .show(ctx, |ui| {
                ui.set_width(width);
                let query_id = id.with("query");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id(query_id)
                        .hint_text(hint)
                        .desired_width(f32::INFINITY),
                );
                response.request_focus();
                if self.select_query {
                    self.select_query = false;
                    if let Some(mut state) = egui::TextEdit::load_state(ctx, query_id) {
                        let all = egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(self.query.chars().count()),
                        );
                        state.cursor.set_char_range(Some(all));
                        state.store(ctx, query_id);
                    }
                }
                if response.changed() {
                    self.selected = 0;
                }
                ui.add_space(4.0);

                let row_height = ui.text_style_height(&TextStyle::Body) + 6.0;
                egui::ScrollArea::vertical()
                    .max_height(row_height * 12.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if items.is_empty() {
                            ui.weak("No matches");
                        }
                        for (idx, item) in items.iter().take(MAX_ROWS).enumerate() {
                            let selected = idx == self.selected;
                            let row = self.row(ui, item, selected, style);
                            if selected && moved {
                                row.scroll_to_me(None);
                            }
                            if row.clicked() {
                                chosen = Some(idx);
                            }
                        }
                    });
                if let Some(text) = footer {
                    ui.add_space(2.0);
                    ui.weak(text);
                }
            });
        if modal.backdrop_response.clicked() {
            self.open = false;
        }
        if chosen.is_some() {
            self.open = false;
        }
        chosen
    }

    fn row(
        &self,
        ui: &mut egui::Ui,
        item: &PickerItem,
        selected: bool,
        style: &PickerStyle,
    ) -> egui::Response {
        let font = TextStyle::Body.resolve(ui.style());
        let small = TextStyle::Small.resolve(ui.style());
        let visuals = ui.visuals().clone();
        let normal = visuals.text_color();
        let mut job = LayoutJob::default();
        let mut highlights = item.highlights.iter().peekable();
        for (i, c) in item.label.chars().enumerate() {
            let hit = highlights.next_if(|&&h| h == i).is_some();
            // The accent would disappear against the selected row's fill.
            let color = match (hit, selected) {
                (true, true) => visuals.strong_text_color(),
                (true, false) => style.highlight,
                (false, _) => normal,
            };
            let mut buf = [0; 4];
            let mut format = TextFormat::simple(font.clone(), color);
            if hit {
                format.underline = egui::Stroke::new(1.0, color);
            }
            job.append(c.encode_utf8(&mut buf), 0.0, format);
        }

        let height = ui.text_style_height(&TextStyle::Body) + 6.0;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click(),
        );
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if selected {
                painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
            } else if response.hovered() {
                painter.rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
            }
            let inner = rect.shrink2(egui::vec2(8.0, 0.0));
            let detail = painter.layout_no_wrap(item.detail.clone(), small, style.muted);
            let detail_width = detail.size().x;
            painter.galley(
                egui::pos2(
                    inner.right() - detail_width,
                    inner.center().y - detail.size().y / 2.0,
                ),
                detail,
                style.muted,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(
                (inner.width() - detail_width - 16.0).max(40.0),
            );
            let label = painter.layout_job(job);
            painter.galley(
                egui::pos2(inner.left(), inner.center().y - label.size().y / 2.0),
                label,
                normal,
            );
        }
        response
    }
}

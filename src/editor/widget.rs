//! The code editor widget.
//!
//! Unlike egui's `TextEdit`, which lays out the whole text every frame, this
//! widget lays out only the rows on screen (and for very long lines, only the
//! visible part of them), reading them straight from the rope. Scrolling,
//! typing and painting therefore cost O(visible text) regardless of file size.
//!
//! It handles cursors (any number of them), selections, folding, word wrap,
//! the keyboard, the mouse, the clipboard and IME input. Everything drawn
//! around the text by the app — find matches, diagnostics, git markers, blame —
//! is positioned using the returned [`Layout`].

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use egui::text::{CCursor, LayoutJob};
use egui::{
    Align, Color32, Event, EventFilter, FontId, Galley, Key, Modifiers, Pos2, Rangef, Rect,
    Response, Sense, Shape, Stroke, Ui, pos2, vec2,
};

use super::folding::FoldSource;
use super::multi::{self, CursorEdit, Transaction};
use super::ops;
use super::selection::{Cursor, Selections};
use super::view::{self, DisplayMap, EditorView, Reveal, cells_to, col_at_cells, line_rows};
use super::{Document, EditKind, Highlighter, gutter};

/// Lines longer than this (in chars) are laid out only around the visible columns.
const LONG_LINE: usize = 2_000;

/// Time spent highlighting per frame before showing what's done.
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(8);

/// Pointer interaction in progress.
#[derive(Debug, Clone, Copy)]
pub(super) struct Drag {
    /// The selection made by the press (a word or line for double / triple click).
    origin: Cursor,
    mode: DragMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Char,
    Word,
    Line,
    /// Alt+Shift drag: a cursor on each line of a rectangle. Holds the
    /// starting line and column (cells).
    Column(usize, usize),
}

/// Recent clicks, to tell double and triple clicks.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Clicks {
    time: f64,
    pos: Pos2,
    count: u32,
}

/// Settings for drawing a view.
pub struct EditorConfig<'a> {
    pub font: FontId,
    pub word_wrap: bool,
    /// Width of the line number gutter (see [`gutter::width`]).
    pub gutter_width: f32,
    /// Width of the blame column left of the gutter (0 when off).
    pub blame_width: f32,
    /// File extension, picking the syntax.
    pub extension: &'a str,
    /// Quote characters that auto-close.
    pub quotes: &'a [char],
    /// Language key for Enter's auto-indent rules.
    pub language: &'a str,
    pub indent_unit: &'a str,
    /// Where the text can fold.
    pub folding: FoldSource<'a>,
    pub selection_color: Color32,
    pub muted_color: Color32,
}

/// One laid-out row on screen.
pub struct Row {
    /// 0-based line.
    pub line: usize,
    /// Whether this is the line's first row (wrapped lines have more).
    pub first: bool,
    /// Whether this is the line's last row.
    pub last: bool,
    /// Global char range shown in this row, without the line break.
    pub chars: Range<usize>,
    /// Screen rect of the row's text (at least the row height tall).
    pub rect: Rect,
    galley: Arc<Galley>,
    /// Chars the galley holds (for very long lines, only the visible part).
    galley_chars: Range<usize>,
    galley_pos: Pos2,
    /// The line's text, for positioning chars outside the galley.
    line_text: Arc<str>,
    line_start: usize,
    /// The row's char range within its line.
    in_line: Range<usize>,
}

impl Row {
    /// Screen x of the caret before global char `c` (clamped to the row).
    fn x_of(&self, c: usize, char_width: f32, text_left: f32) -> f32 {
        let c = c.clamp(self.chars.start, self.chars.end);
        if self.galley_chars.contains(&c) || c == self.galley_chars.end {
            let local = c - self.galley_chars.start;
            self.galley_pos.x + self.galley.pos_from_cursor(CCursor::new(local)).min.x
        } else {
            text_left
                + cells_to(&self.line_text, self.in_line.clone(), c - self.line_start) as f32
                    * char_width
        }
    }

    /// The char boundary nearest to screen x.
    fn char_at_x(&self, x: f32, char_width: f32, text_left: f32) -> usize {
        let galley_left = self.galley_pos.x;
        let galley_right = galley_left + self.galley.size().x;
        if !self.galley_chars.is_empty()
            && (x >= galley_left || self.galley_chars.start == self.chars.start)
            && (x <= galley_right || self.galley_chars.end == self.chars.end)
        {
            let local = self
                .galley
                .cursor_from_pos(vec2(x - galley_left, self.galley.size().y / 2.0));
            return self.galley_chars.start + local.index.0;
        }
        let goal = ((x - text_left) / char_width).round().max(0.0) as usize;
        let last = self.chars.end == self.line_start + self.line_text.chars().count();
        self.line_start + col_at_cells(&self.line_text, self.in_line.clone(), goal, last)
    }
}

/// Where everything is on screen this frame.
pub struct Layout {
    pub rows: Vec<Row>,
    /// The gutter, pinned to the left edge as the text scrolls sideways.
    pub gutter: Rect,
    /// The blame column, if shown.
    pub blame: Option<Rangef>,
    /// Where text is drawn (right of the gutter).
    pub text_clip: Rect,
    /// Screen x of column 0.
    pub text_left: f32,
    pub row_height: f32,
    pub char_width: f32,
    /// Chars on the visible rows.
    pub visible_chars: Range<usize>,
    /// A shape slot under the text for the app's highlights (find matches, …).
    pub underlay: egui::layers::ShapeIdx,
}

impl Layout {
    /// (line, first row of the line, screen rect) for each visible row.
    pub fn row_rects(&self) -> impl Iterator<Item = (usize, bool, Rect)> + '_ {
        self.rows.iter().map(|r| (r.line, r.first, r.rect))
    }

    fn row_of(&self, c: usize) -> Option<&Row> {
        // The row containing `c`; a char at a wrap point belongs to the next row.
        let i = self.rows.partition_point(|r| r.chars.start <= c);
        let row = self.rows.get(i.checked_sub(1)?)?;
        (c <= row.chars.end).then_some(row)
    }

    /// Screen rect of the caret before char `c`, if it is on screen.
    pub fn caret_rect(&self, c: usize) -> Option<Rect> {
        let row = self.row_of(c)?;
        let x = row.x_of(c, self.char_width, self.text_left);
        Some(Rect::from_min_max(
            pos2(x, row.rect.top()),
            pos2(x, row.rect.top() + self.row_height),
        ))
    }

    /// Rects covering the chars in `range`, one per visible row it touches.
    pub fn range_rects(&self, range: Range<usize>) -> Vec<Rect> {
        let mut rects = Vec::new();
        for row in &self.rows {
            if row.chars.start > range.end || row.chars.end < range.start {
                continue;
            }
            let a = range.start.max(row.chars.start);
            let b = range.end.min(row.chars.end);
            // A selected line break shows as a little extra width.
            let newline = row.last && range.end > row.chars.end;
            if a == b && !newline && !range.is_empty() {
                continue;
            }
            let left = row.x_of(a, self.char_width, self.text_left);
            let mut right = row.x_of(b, self.char_width, self.text_left);
            if newline {
                right += self.char_width * 0.5;
            }
            right = right.max(left + 4.0_f32.min(self.char_width));
            rects.push(Rect::from_min_max(
                pos2(left, row.rect.top()),
                pos2(right, row.rect.top() + self.row_height),
            ));
        }
        rects
    }

    /// The char under screen `pos` and its rect, if the pointer is over a
    /// character rather than empty space.
    pub fn char_at(&self, pos: Pos2) -> Option<(usize, Rect)> {
        if !self.text_clip.contains(pos) {
            return None;
        }
        let row = self
            .rows
            .iter()
            .find(|r| (r.rect.top()..r.rect.top() + self.row_height).contains(&pos.y))?;
        let near = row.char_at_x(pos.x, self.char_width, self.text_left);
        [near.saturating_sub(1), near].into_iter().find_map(|c| {
            if c < row.chars.start || c >= row.chars.end {
                return None;
            }
            let a = row.x_of(c, self.char_width, self.text_left);
            let b = row.x_of(c + 1, self.char_width, self.text_left);
            let r = Rect::from_min_max(
                pos2(a, row.rect.top()),
                pos2(b, row.rect.top() + self.row_height),
            );
            (b > a && r.contains(pos)).then_some((c, r))
        })
    }

    /// Screen rect of all visible rows of `line`, full editor width.
    pub fn line_rect(&self, line: usize) -> Option<Rect> {
        self.rows
            .iter()
            .filter(|r| r.line == line)
            .map(|r| {
                Rect::from_x_y_ranges(
                    self.text_clip.x_range(),
                    r.rect.top()..=r.rect.top() + self.row_height,
                )
            })
            .reduce(|a, b| a.union(b))
    }
}

/// What happened in the editor this frame.
pub struct EditorOutput<R> {
    pub response: Response,
    pub layout: Layout,
    /// The text was edited.
    pub changed: bool,
    /// The last character typed, if any (for completion triggers).
    pub typed: Option<char>,
    /// Screen rect of the primary caret, when the editor has focus and it is visible.
    pub caret: Option<(usize, Rect)>,
    pub inner: R,
}

pub struct EditorWidget<'a> {
    pub doc: &'a mut Document,
    pub view: &'a mut EditorView,
    pub highlighter: &'a Highlighter,
    pub config: EditorConfig<'a>,
}

/// Frame-to-frame geometry, needed before laying out (for keyboard paging
/// and mouse hits).
#[derive(Clone, Copy)]
struct Metrics {
    row_height: f32,
    char_width: f32,
}

impl EditorWidget<'_> {
    /// Shows the editor filling the available space. `decorate` runs inside
    /// the scroll area after the text is laid out, to paint around it.
    pub fn show<R>(
        self,
        ui: &mut Ui,
        decorate: impl FnOnce(&mut Ui, &Layout, &Document) -> R,
    ) -> EditorOutput<R> {
        let EditorWidget {
            doc,
            view,
            highlighter,
            config,
        } = self;
        let metrics = ui.fonts_mut(|f| Metrics {
            row_height: f.row_height(&config.font),
            char_width: f.glyph_width(&config.font, ' '),
        });
        view.sync(&doc.buffer);
        let id = view.id;

        let mut changed = false;
        let mut typed = None;
        let has_focus = ui.memory(|m| m.has_focus(id));
        let filter = EventFilter {
            tab: true,
            horizontal_arrows: true,
            vertical_arrows: true,
            escape: true,
        };
        if has_focus {
            ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
        }

        let scroll = egui::ScrollArea::both()
            .id_salt(id)
            .auto_shrink(false)
            .show_viewport(ui, |ui, viewport| {
                let clip = ui.clip_rect();
                let origin = ui.max_rect().min;
                let side = config.blame_width + config.gutter_width;
                let text_width = (clip.width() - side).max(metrics.char_width * 8.0);
                let wrap_cols = config
                    .word_wrap
                    .then(|| {
                        ((text_width - metrics.char_width) / metrics.char_width).floor() as usize
                    })
                    .map(|c| c.max(8));
                let page_rows = ((clip.height() / metrics.row_height) as isize - 1).max(1);

                // Keyboard first, so this frame shows its result.
                if has_focus {
                    let events = ui.input(|i| i.filtered_events(&filter));
                    let input = Input {
                        doc: &mut *doc,
                        view: &mut *view,
                        config: &config,
                        wrap_cols,
                        page_rows,
                        mac: ui.os() == egui::os::OperatingSystem::Mac,
                    };
                    let (c, t) = input.handle_events(ui, &events);
                    changed |= c;
                    typed = t.or(typed);
                }

                let map = view.display_map(&doc.buffer, wrap_cols).clone();
                let height = (map.total_rows() + 2) as f32 * metrics.row_height;
                let width = side + view.max_width.max(text_width);
                let (content, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                let visible = content.intersect(clip);
                let response = ui.interact(visible, id, Sense::click_and_drag());

                let geometry = Geometry {
                    origin,
                    clip,
                    side,
                    blame_width: config.blame_width,
                    gutter_width: config.gutter_width,
                    metrics,
                    wrap_cols,
                };

                // Mouse, against this frame's text.
                let pointer_changed =
                    handle_pointer(ui, &response, &geometry, &*doc, &mut *view, &map, &config);
                if pointer_changed {
                    ui.memory_mut(|m| m.request_focus(id));
                }
                let map = view.display_map(&doc.buffer, wrap_cols).clone();

                // Scroll a moved cursor into view.
                if let Some(reveal) = view.reveal.take() {
                    reveal_cursor(ui, &geometry, &doc.buffer, view, &map, reveal);
                }

                // Highlight what's visible, a little ahead.
                let first_row = (viewport.top() / metrics.row_height).floor().max(0.0) as usize;
                let last_row = (viewport.bottom() / metrics.row_height).ceil() as usize;
                let (last_line, _) = map.line_at_row(last_row);
                if !highlighter.highlight(
                    &mut doc.highlight,
                    &doc.buffer,
                    config.extension,
                    last_line + 2,
                    HIGHLIGHT_BUDGET,
                ) {
                    ui.ctx().request_repaint();
                }

                let underlay = ui
                    .painter()
                    .with_clip_rect(text_clip(&geometry))
                    .add(Shape::Noop);
                let layout = lay_out(
                    ui,
                    &geometry,
                    &*doc,
                    view,
                    highlighter,
                    &config,
                    &map,
                    first_row..last_row + 1,
                    viewport,
                    underlay,
                );
                paint(ui, &layout, &doc.buffer, view, &config, has_focus);
                let inner = decorate(ui, &layout, &*doc);
                (response, layout, inner)
            });
        let (response, layout, inner) = scroll.inner;

        let primary = view.sel.primary();
        let caret = has_focus
            .then(|| layout.caret_rect(primary.head))
            .flatten()
            .filter(|r| layout.text_clip.intersects(*r))
            .map(|r| (primary.head, r));
        if let Some((_, r)) = caret {
            let to_global = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            ui.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: to_global * layout.text_clip,
                    cursor_rect: to_global * r,
                    should_interrupt_composition: false,
                });
            });
        }
        EditorOutput {
            response,
            layout,
            changed,
            typed,
            caret,
            inner,
        }
    }
}

/// Where the parts of the widget are, in screen space.
struct Geometry {
    /// Screen position of the content's top-left (moves as it scrolls).
    origin: Pos2,
    /// The visible area.
    clip: Rect,
    /// Blame + gutter width.
    side: f32,
    blame_width: f32,
    gutter_width: f32,
    metrics: Metrics,
    wrap_cols: Option<usize>,
}

impl Geometry {
    fn text_left(&self) -> f32 {
        self.origin.x + self.side
    }

    fn row_top(&self, row: usize) -> f32 {
        self.origin.y + row as f32 * self.metrics.row_height
    }

    fn gutter(&self) -> Rect {
        let left = self.clip.left() + self.blame_width;
        Rect::from_x_y_ranges(left..=left + self.gutter_width, self.clip.y_range())
    }
}

fn text_clip(g: &Geometry) -> Rect {
    Rect::from_x_y_ranges(g.clip.left() + g.side..=g.clip.right(), g.clip.y_range())
}

/// Lays out the display rows in `rows` (those intersecting the viewport).
#[allow(clippy::too_many_arguments)]
fn lay_out(
    ui: &Ui,
    g: &Geometry,
    doc: &Document,
    view: &mut EditorView,
    highlighter: &Highlighter,
    config: &EditorConfig<'_>,
    map: &DisplayMap,
    rows: Range<usize>,
    viewport: Rect,
    underlay: egui::layers::ShapeIdx,
) -> Layout {
    let buf = &doc.buffer;
    let m = g.metrics;
    let text_left = g.text_left();
    let mut out = Vec::new();
    let total = map.total_rows();
    if total > 0 && rows.start < total {
        let (mut line, mut row_in_line) = map.line_at_row(rows.start);
        let mut row = rows.start;
        while row < rows.end && line < buf.line_count() {
            let text: Arc<str> = buf.line_text(line).into();
            let line_start = buf.line_start(line);
            let parts = line_rows(&text, g.wrap_cols);
            let spans = doc.highlight.spans(line);
            // Byte offset of each char boundary is needed per row; walk once.
            for (i, part) in parts.iter().enumerate().skip(row_in_line) {
                if row >= rows.end {
                    break;
                }
                let top = g.row_top(row);
                // For long unwrapped lines, only the visible columns.
                let shown = if g.wrap_cols.is_none() && part.len() > LONG_LINE {
                    let first_col = ((viewport.left() / m.char_width) as usize).saturating_sub(64);
                    let last_col = (viewport.right() / m.char_width) as usize + 64;
                    let a = col_at_cells(&text, part.clone(), first_col, true);
                    let b = col_at_cells(&text, part.clone(), last_col, true);
                    a..b.max(a)
                } else {
                    part.clone()
                };
                let byte_a = char_to_byte(&text, shown.start);
                let byte_b = char_to_byte(&text, shown.end);
                let mut job = LayoutJob::default();
                highlighter.append(&mut job, &text[byte_a..byte_b], byte_a, spans, &config.font);
                job.wrap.max_width = f32::INFINITY;
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                let gx =
                    text_left + cells_to(&text, part.clone(), shown.start) as f32 * m.char_width;
                let right = gx + galley.size().x;
                if g.wrap_cols.is_none() {
                    let full = if part.len() > LONG_LINE {
                        text_left + (cells_to(&text, part.clone(), part.end) as f32) * m.char_width
                    } else {
                        right
                    };
                    view.max_width = view.max_width.max(full - text_left + m.char_width * 4.0);
                }
                out.push(Row {
                    line,
                    first: i == 0,
                    last: i + 1 == parts.len(),
                    chars: line_start + part.start..line_start + part.end,
                    rect: Rect::from_min_max(
                        pos2(text_left, top),
                        pos2(right.max(text_left), top + m.row_height),
                    ),
                    galley,
                    galley_chars: line_start + shown.start..line_start + shown.end,
                    galley_pos: pos2(gx, top),
                    line_text: text.clone(),
                    line_start,
                    in_line: part.clone(),
                });
                row += 1;
            }
            row_in_line = 0;
            line = next_visible_line(view, line);
        }
    }
    let visible_chars = match (out.first(), out.last()) {
        (Some(a), Some(b)) => a.chars.start..b.chars.end,
        _ => 0..0,
    };
    Layout {
        rows: out,
        gutter: g.gutter(),
        blame: (g.blame_width > 0.0).then(|| {
            let left = g.clip.left();
            Rangef::new(left, left + g.blame_width)
        }),
        text_clip: text_clip(g),
        text_left,
        row_height: m.row_height,
        char_width: m.char_width,
        visible_chars,
        underlay,
    }
}

/// The next line after `line` that isn't folded away.
fn next_visible_line(view: &EditorView, line: usize) -> usize {
    let end = view
        .folds()
        .iter()
        .filter(|f| f.start == line)
        .map(|f| f.end)
        .max()
        .unwrap_or(line);
    end + 1
}

fn char_to_byte(text: &str, c: usize) -> usize {
    text.char_indices().nth(c).map_or(text.len(), |(b, _)| b)
}

fn paint(
    ui: &Ui,
    layout: &Layout,
    buf: &super::Buffer,
    view: &EditorView,
    config: &EditorConfig<'_>,
    focused: bool,
) {
    let painter = ui.painter().with_clip_rect(layout.text_clip);
    let visuals = ui.visuals();

    // Selections.
    let fill = if focused {
        config.selection_color
    } else {
        config.selection_color.gamma_multiply(0.5)
    };
    let mut shapes = Vec::new();
    let visible = layout.visible_chars.clone();
    for c in view.sel.iter().filter(|c| !c.is_empty()) {
        if c.end() < visible.start || c.start() > visible.end {
            continue;
        }
        for r in layout.range_rects(c.range()) {
            shapes.push(Shape::rect_filled(r, 2.0, fill));
        }
    }
    painter.extend(shapes);

    // Text.
    for row in &layout.rows {
        painter.galley(row.galley_pos, row.galley.clone(), visuals.text_color());
    }

    // Folded placeholders.
    for row in &layout.rows {
        let last_row_of_line = row.chars.end == row.line_start + row.line_text.chars().count();
        if last_row_of_line && view.is_folded_at(row.line) {
            let x =
                row.x_of(row.chars.end, layout.char_width, layout.text_left) + layout.char_width;
            let r = Rect::from_min_size(
                pos2(x, row.rect.top() + 2.0),
                vec2(layout.char_width * 3.0, layout.row_height - 4.0),
            );
            painter.rect_filled(r, 3.0, config.muted_color.gamma_multiply(0.25));
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "⋯",
                config.font.clone(),
                config.muted_color,
            );
        }
    }

    // Carets.
    if focused {
        let style = &visuals.text_cursor;
        let now = ui.input(|i| i.time);
        let since = (now - view.last_activity) as f32;
        let period = style.on_duration + style.off_duration;
        let on = !style.blink || since < style.on_duration || since % period < style.on_duration;
        if style.blink {
            let into = since % period;
            let wait = if into < style.on_duration {
                style.on_duration - into
            } else {
                period - into
            };
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f32(wait.max(0.01)));
        }
        if on {
            for c in view.sel.iter() {
                if let Some(r) = layout.caret_rect(c.head) {
                    let x = r.left().max(layout.text_clip.left() + 1.0);
                    painter.line_segment(
                        [pos2(x, r.top()), pos2(x, r.bottom())],
                        Stroke::new(style.stroke.width.max(1.5), style.stroke.color),
                    );
                }
            }
        }
        // IME composition, drawn over the text at the caret.
        if let Some(text) = view.preedit.as_ref().filter(|t| !t.is_empty())
            && let Some(r) = layout.caret_rect(view.sel.primary().head)
        {
            let galley =
                painter.layout_no_wrap(text.clone(), config.font.clone(), visuals.text_color());
            let bg = Rect::from_min_size(r.min, vec2(galley.size().x, layout.row_height));
            painter.rect_filled(bg, 0.0, visuals.extreme_bg_color);
            painter.galley(r.min, galley, visuals.text_color());
            painter.hline(
                bg.x_range(),
                bg.bottom() - 1.0,
                Stroke::new(1.0, visuals.text_color()),
            );
        }
    }

    // Fold markers in the gutter.
    let hovering_gutter = ui.rect_contains_pointer(layout.gutter);
    let gutter_painter = ui.painter().with_clip_rect(layout.gutter);
    for (line, first, rect) in layout.row_rects() {
        if !first {
            continue;
        }
        let folded = view.is_folded_at(line);
        if folded || (hovering_gutter && config.folding.foldable(buf, line)) {
            gutter::paint_fold_marker(
                &gutter_painter,
                layout.gutter,
                rect,
                folded,
                config.muted_color,
            );
        }
    }
}

/// Scrolls the primary cursor into view.
fn reveal_cursor(
    ui: &Ui,
    g: &Geometry,
    buf: &super::Buffer,
    view: &EditorView,
    map: &DisplayMap,
    reveal: Reveal,
) {
    let head = view.sel.primary().head;
    let line = buf.char_to_line(head);
    let text = buf.line_text(line);
    let col = head - buf.line_start(line);
    let parts = line_rows(&text, g.wrap_cols);
    let r = parts.iter().rposition(|p| p.start <= col).unwrap_or(0);
    let row = map.row_of_line(line) + r;
    let x = g.text_left() + cells_to(&text, parts[r].clone(), col) as f32 * g.metrics.char_width;
    let top = g.row_top(row);
    let m = g.metrics;
    let rect = Rect::from_min_max(
        pos2(x - g.side - m.char_width * 2.0, top - m.row_height),
        pos2(x + m.char_width * 4.0, top + m.row_height * 2.0),
    );
    let align = match reveal {
        Reveal::Cursor => None,
        Reveal::Center => Some(Align::Center),
    };
    if reveal == Reveal::Center {
        // Center vertically only; keep the left edge (gutter) in view.
        ui.scroll_to_rect_animation(
            Rect::from_min_max(
                pos2(x - g.side, top),
                pos2(x + m.char_width, top + m.row_height),
            ),
            align,
            egui::style::ScrollAnimation::none(),
        );
    } else {
        // Jump straight there, as editors do; animating a long jump crawls.
        ui.scroll_to_rect_animation(rect, align, egui::style::ScrollAnimation::none());
    }
}

// ---- mouse ---------------------------------------------------------------

/// The char position under screen `pos`, anywhere in the document (rows off
/// screen are measured, not laid out).
fn hit(g: &Geometry, buf: &super::Buffer, map: &DisplayMap, pos: Pos2) -> usize {
    let total = map.total_rows();
    if total == 0 {
        return 0;
    }
    let y = pos.y - g.origin.y;
    if y < 0.0 {
        return 0;
    }
    let row = (y / g.metrics.row_height) as usize;
    if row >= total {
        return buf.len_chars();
    }
    let (line, r) = map.line_at_row(row);
    let text = buf.line_text(line);
    let parts = line_rows(&text, g.wrap_cols);
    let r = r.min(parts.len() - 1);
    let goal = ((pos.x - g.text_left()) / g.metrics.char_width)
        .round()
        .max(0.0) as usize;
    buf.line_start(line) + col_at_cells(&text, parts[r].clone(), goal, r + 1 == parts.len())
}

/// The line and column (cells) under `pos`, for column selection.
fn hit_cell(g: &Geometry, map: &DisplayMap, pos: Pos2) -> (usize, usize) {
    let row = ((pos.y - g.origin.y) / g.metrics.row_height).max(0.0) as usize;
    let (line, _) = map.line_at_row(row);
    let col = ((pos.x - g.text_left()) / g.metrics.char_width)
        .round()
        .max(0.0) as usize;
    (line, col)
}

/// Handles clicks and drags. Returns whether the editor should take focus.
fn handle_pointer(
    ui: &Ui,
    response: &Response,
    g: &Geometry,
    doc: &Document,
    view: &mut EditorView,
    map: &DisplayMap,
    config: &EditorConfig<'_>,
) -> bool {
    let buf = &doc.buffer;
    let (pos, pressed, down, released, modifiers, time) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.modifiers,
            i.time,
        )
    });
    let Some(pos) = pos else {
        return false;
    };
    let gutter = g.gutter();
    let text_area = text_clip(g);
    if response.hovered() && text_area.contains(pos) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }

    if pressed && response.hovered() {
        // Fold markers and folded placeholders toggle folds.
        if gutter.contains(pos) && gutter::fold_marker_hit(gutter, pos) {
            let at = hit(g, buf, map, pos2(g.text_left(), pos.y));
            let line = buf.char_to_line(at);
            if !view.unfold(line)
                && let Some((s, e)) = config.folding.range_at(buf, line)
            {
                view.fold(buf, s, e);
            }
            return false;
        }
        let at = hit(g, buf, map, pos);
        let line = buf.char_to_line(at);
        if view.is_folded_at(line) && text_area.contains(pos) {
            // The "⋯" placeholder after the line's text.
            let len = buf.line_len(line);
            let cells = cells_to(&buf.line_text(line), 0..len, len) as f32;
            let x0 = g.text_left() + (cells + 1.0) * g.metrics.char_width;
            if (x0..=x0 + 3.0 * g.metrics.char_width).contains(&pos.x) {
                view.unfold(line);
                return true;
            }
        }

        // Click counting for double / triple clicks.
        let c = &mut view.clicks;
        if time - c.time < 0.4 && c.pos.distance(pos) < 6.0 {
            c.count = c.count % 3 + 1;
        } else {
            c.count = 1;
        }
        c.time = time;
        c.pos = pos;
        let count = c.count;

        // Alt (Option) + click adds a cursor; with Shift it starts a column selection.
        let add = modifiers.alt;
        if gutter.contains(pos) {
            // Clicking line numbers selects lines.
            let c = view::line_selection(buf, line);
            let origin = if modifiers.shift {
                let anchor = view.sel.primary().anchor;
                if c.start() >= anchor {
                    Cursor::new(anchor, c.end())
                } else {
                    Cursor::new(anchor, c.start())
                }
            } else {
                c
            };
            view.sel = Selections::single(origin);
            view.drag = Some(Drag {
                origin,
                mode: DragMode::Line,
            });
        } else if add && modifiers.shift {
            let (l, col) = hit_cell(g, map, pos);
            view.drag = Some(Drag {
                origin: Cursor::caret(at),
                mode: DragMode::Column(l, col),
            });
            column_select(buf, view, l, col, l, col);
        } else {
            let (origin, mode) = match count {
                2 => {
                    let w = view::word_range(buf, at);
                    (Cursor::new(w.start, w.end), DragMode::Word)
                }
                3 => (view::line_selection(buf, line), DragMode::Line),
                _ => (Cursor::caret(at), DragMode::Char),
            };
            if add && count == 1 {
                // Alt+click on an existing caret removes it.
                let existing = view.sel.iter().position(|c| c.is_empty() && c.head == at);
                if let Some(i) = existing
                    && view.sel.is_multi()
                {
                    view.sel.remove(i);
                    view.drag = None;
                    view.last_activity = time;
                    return true;
                }
                view.sel.add(origin);
            } else if add {
                view.sel.set_primary(origin);
            } else if modifiers.shift {
                let anchor = view.sel.primary().anchor;
                let c = Cursor::new(anchor, at);
                view.sel = Selections::single(c);
                view.drag = Some(Drag { origin: c, mode });
                view.last_activity = time;
                return true;
            } else {
                view.sel = Selections::single(origin);
            }
            view.drag = Some(Drag { origin, mode });
        }
        view.last_activity = time;
        return true;
    }

    if let Some(drag) = view.drag {
        if !down || released {
            view.drag = None;
            return false;
        }
        if response.dragged() || ui.input(|i| i.pointer.is_decidedly_dragging()) {
            match drag.mode {
                DragMode::Column(l0, c0) => {
                    let (l1, c1) = hit_cell(g, map, pos);
                    column_select(buf, view, l0, c0, l1, c1);
                }
                mode => {
                    let at = hit(g, buf, map, pos);
                    let o = drag.origin;
                    let span = match mode {
                        DragMode::Word => view::word_range(buf, at),
                        DragMode::Line => {
                            let c = view::line_selection(buf, buf.char_to_line(at));
                            c.start()..c.end()
                        }
                        _ => at..at,
                    };
                    let c = if span.start < o.start() {
                        Cursor::new(o.end(), span.start)
                    } else {
                        Cursor::new(o.start(), span.end.max(o.end()))
                    };
                    let c = if mode == DragMode::Char {
                        Cursor::new(o.anchor, at)
                    } else {
                        c
                    };
                    view.sel.set_primary(c);
                }
            }
            view.last_activity = time;
            // Scroll when dragging past the top or bottom edge.
            let clip = g.clip;
            let dy = if pos.y < clip.top() {
                clip.top() - pos.y
            } else if pos.y > clip.bottom() {
                clip.bottom() - pos.y
            } else {
                0.0
            };
            if dy != 0.0 {
                ui.scroll_with_delta(vec2(0.0, dy.clamp(-40.0, 40.0)));
                ui.ctx().request_repaint();
            }
        }
    }
    false
}

/// Puts a cursor on each line from `l0` to `l1`, spanning columns `c0..c1`.
fn column_select(
    buf: &super::Buffer,
    view: &mut EditorView,
    l0: usize,
    c0: usize,
    l1: usize,
    c1: usize,
) {
    let (top, bottom) = (l0.min(l1), l0.max(l1));
    let mut cursors = Vec::new();
    for line in top..=bottom {
        if view.is_hidden(line) {
            continue;
        }
        let text = buf.line_text(line);
        let len = text.chars().count();
        let width = cells_to(&text, 0..len, len);
        if width < c0.min(c1) && line != l1 {
            continue;
        }
        let start = buf.line_start(line);
        let a = col_at_cells(&text, 0..len, c0, true);
        let b = col_at_cells(&text, 0..len, c1, true);
        cursors.push(Cursor::new(start + a, start + b));
    }
    let primary = if l1 >= l0 {
        cursors.len().saturating_sub(1)
    } else {
        0
    };
    view.sel = Selections::new(cursors, primary);
}

// ---- keyboard ------------------------------------------------------------

struct Input<'a, 'c> {
    doc: &'a mut Document,
    view: &'a mut EditorView,
    config: &'a EditorConfig<'c>,
    wrap_cols: Option<usize>,
    page_rows: isize,
    mac: bool,
}

impl Input<'_, '_> {
    /// Handles keys, text, clipboard and IME events. Returns whether the
    /// text changed and the last char typed.
    fn handle_events(mut self, ui: &Ui, events: &[Event]) -> (bool, Option<char>) {
        let mut changed = false;
        let mut typed = None;
        let time = ui.input(|i| i.time);
        for event in events {
            let before = self.view.sel.clone();
            let edited = match event {
                Event::Text(t) if !t.is_empty() && t != "\n" && t != "\r" => {
                    let mut chars = t.chars();
                    if let (Some(c), None) = (chars.next(), chars.next()) {
                        typed = Some(c);
                    }
                    self.type_text(t)
                }
                Event::Paste(t) if !t.is_empty() => self.paste(t),
                Event::Copy => {
                    ui.ctx().copy_text(self.copied_text());
                    false
                }
                Event::Cut => {
                    ui.ctx().copy_text(self.copied_text());
                    self.cut()
                }
                Event::Ime(ime) => self.ime(ime),
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => self.key(*key, *modifiers),
                _ => false,
            };
            changed |= edited;
            if edited || self.view.sel != before {
                self.view.last_activity = time;
                if self.view.reveal.is_none() {
                    self.view.reveal = Some(Reveal::Cursor);
                }
            }
        }
        (changed, typed)
    }

    fn buf(&self) -> &super::Buffer {
        &self.doc.buffer
    }

    /// Applies a multi-cursor transaction as an undo step of `kind`.
    fn commit(&mut self, tx: Transaction, kind: EditKind) -> bool {
        let changed = tx.changes_text();
        if changed {
            let before = self.view.sel.snapshot();
            self.doc
                .buffer
                .edit(tx.edits, kind, Some((before, tx.after.snapshot())));
        }
        self.view.sync(&self.doc.buffer);
        self.view.sel = tx.after;
        self.view.unfold_cursors(&self.doc.buffer);
        changed
    }

    fn each(
        &mut self,
        kind: EditKind,
        f: impl FnMut(&super::Buffer, &Cursor) -> CursorEdit,
    ) -> bool {
        let tx = multi::for_each_cursor(&self.doc.buffer, &self.view.sel, f);
        self.commit(tx, kind)
    }

    fn type_text(&mut self, t: &str) -> bool {
        let quotes = self.config.quotes;
        let single = {
            let mut chars = t.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            }
        };
        self.view.preedit = None;
        self.each(EditKind::Typing, |b, c| {
            single
                .and_then(|ch| {
                    multi::local(b, c, 100, 0, |text, s| ops::type_char(text, s, ch, quotes))
                })
                .unwrap_or_else(|| CursorEdit::insert(c.range(), t))
        })
    }

    fn paste(&mut self, t: &str) -> bool {
        let t = t.replace("\r\n", "\n");
        let lines: Vec<&str> = t.strip_suffix('\n').unwrap_or(&t).split('\n').collect();
        let n = self.view.sel.len();
        // One line per cursor when the counts match (copied from as many cursors).
        let per_cursor = n > 1 && lines.len() == n;
        let mut i = 0;
        let whole_lines = self.view.sel.iter().all(Cursor::is_empty)
            && t.ends_with('\n')
            && !per_cursor
            && n == 1
            && lines.len() == 1;
        let changed = self.each(EditKind::Command, |b, c| {
            let text = if per_cursor { lines[i] } else { t.as_str() };
            i += 1;
            if whole_lines {
                // A line copied without a selection goes above the current line.
                let start = b.line_start(b.char_to_line(c.head));
                return CursorEdit::Replace {
                    range: start..start,
                    text: text.to_owned(),
                    cursor: Cursor::caret(c.head + text.chars().count()),
                };
            }
            CursorEdit::insert(c.range(), text)
        });
        self.doc.buffer.seal();
        changed
    }

    /// The selected text, one cursor per line; without a selection, the
    /// cursors' whole lines.
    fn copied_text(&self) -> String {
        let buf = self.buf();
        if self.view.sel.iter().all(Cursor::is_empty) {
            let mut lines: Vec<usize> = self
                .view
                .sel
                .iter()
                .map(|c| buf.char_to_line(c.head))
                .collect();
            lines.dedup();
            return lines
                .into_iter()
                .map(|l| {
                    let c = view::line_selection(buf, l);
                    let mut s = buf.slice(c.range());
                    if !s.ends_with('\n') {
                        s.push('\n');
                    }
                    s
                })
                .collect();
        }
        let parts: Vec<String> = self
            .view
            .sel
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| buf.slice(c.range()))
            .collect();
        parts.join("\n")
    }

    fn cut(&mut self) -> bool {
        let lines = self.view.sel.iter().all(Cursor::is_empty);
        let changed = self.each(EditKind::Command, |b, c| {
            if lines {
                let l = view::line_selection(b, b.char_to_line(c.head));
                CursorEdit::insert(l.range(), "")
            } else {
                CursorEdit::insert(c.range(), "")
            }
        });
        self.doc.buffer.seal();
        changed
    }

    fn ime(&mut self, ime: &egui::ImeEvent) -> bool {
        match ime {
            egui::ImeEvent::Preedit { text, .. } => {
                self.view.preedit = (!text.is_empty()).then(|| text.clone());
                false
            }
            egui::ImeEvent::Commit(text) => {
                self.view.preedit = None;
                !text.is_empty() && self.type_text(text)
            }
            _ => false,
        }
    }

    fn key(&mut self, key: Key, m: Modifiers) -> bool {
        let word = if self.mac { m.alt } else { m.ctrl };
        let line_mod = self.mac && m.mac_cmd;
        let shift = m.shift;
        match key {
            Key::ArrowLeft | Key::ArrowRight => {
                let right = key == Key::ArrowRight;
                let buf = &self.doc.buffer;
                self.view.sel.update(|c| {
                    let to = if !shift && !c.is_empty() && !word && !line_mod {
                        if right { c.end() } else { c.start() }
                    } else if line_mod {
                        if right {
                            line_end(buf, c.head)
                        } else {
                            view::smart_home(buf, c.head)
                        }
                    } else if word {
                        if right {
                            view::word_right(buf, c.head)
                        } else {
                            view::word_left(buf, c.head)
                        }
                    } else if right {
                        (c.head + 1).min(buf.len_chars())
                    } else {
                        c.head.saturating_sub(1)
                    };
                    c.move_to(to, shift);
                    c.goal = None;
                });
                self.skip_folds(right);
                false
            }
            Key::ArrowUp | Key::ArrowDown => {
                let down = key == Key::ArrowDown;
                if line_mod {
                    let to = if down { self.buf().len_chars() } else { 0 };
                    self.move_all(to, shift);
                } else if !(m.alt || m.ctrl || m.command) {
                    self.vertical(if down { 1 } else { -1 }, shift);
                }
                false
            }
            Key::PageUp | Key::PageDown => {
                let rows = if key == Key::PageDown {
                    self.page_rows
                } else {
                    -self.page_rows
                };
                self.vertical(rows, shift);
                false
            }
            Key::Home | Key::End => {
                let end = key == Key::End;
                if m.command {
                    let to = if end { self.buf().len_chars() } else { 0 };
                    self.move_all(to, shift);
                } else {
                    let buf = &self.doc.buffer;
                    self.view.sel.update(|c| {
                        let to = if end {
                            line_end(buf, c.head)
                        } else {
                            view::smart_home(buf, c.head)
                        };
                        c.move_to(to, shift);
                        c.goal = None;
                    });
                }
                false
            }
            Key::A if m.command && !shift => {
                let len = self.buf().len_chars();
                self.view.sel = Selections::single(Cursor::new(0, len));
                false
            }
            Key::Escape if self.view.sel.is_multi() => {
                self.view.sel.collapse_to_primary();
                false
            }
            Key::Enter if !m.command => {
                let unit = self.config.indent_unit;
                let lang = self.config.language;
                self.each(EditKind::Typing, |b, c| {
                    multi::local(b, c, 0, 0, |t, s| Some(ops::newline(t, s, unit, lang)))
                        .unwrap_or_else(|| CursorEdit::insert(c.range(), "\n"))
                })
            }
            Key::Backspace => {
                let quotes = self.config.quotes;
                let to_line_start = line_mod;
                self.each(EditKind::Typing, |b, c| {
                    if !c.is_empty() {
                        return CursorEdit::insert(c.range(), "");
                    }
                    let from = if to_line_start {
                        let start = b.line_start(b.char_to_line(c.head));
                        if start == c.head {
                            c.head.saturating_sub(1)
                        } else {
                            start
                        }
                    } else if word {
                        view::word_left(b, c.head)
                    } else {
                        if let Some(e) =
                            multi::local(b, c, 0, 0, |t, s| ops::backspace_pair(t, s, quotes))
                        {
                            return e;
                        }
                        c.head.saturating_sub(1)
                    };
                    CursorEdit::insert(from..c.head, "")
                })
            }
            Key::Delete => self.each(EditKind::Typing, |b, c| {
                if !c.is_empty() {
                    return CursorEdit::insert(c.range(), "");
                }
                let to = if word {
                    view::word_right(b, c.head)
                } else if line_mod {
                    line_end(b, c.head)
                } else {
                    (c.head + 1).min(b.len_chars())
                };
                CursorEdit::insert(c.head..to, "")
            }),
            Key::Tab if !m.command && !m.ctrl && !m.alt => {
                let unit = self.config.indent_unit;
                let buf = &self.doc.buffer;
                let multi_line = self
                    .view
                    .sel
                    .iter()
                    .any(|c| buf.char_to_line(c.start()) != buf.char_to_line(c.end()));
                if shift {
                    self.each(EditKind::Command, |b, c| {
                        multi::local(b, c, 0, 0, |t, s| Some(ops::outdent(t, s, unit)))
                            .unwrap_or(CursorEdit::Keep)
                    })
                } else if multi_line {
                    self.each(EditKind::Command, |b, c| {
                        multi::local(b, c, 0, 0, |t, s| Some(ops::indent(t, s, unit)))
                            .unwrap_or(CursorEdit::Keep)
                    })
                } else {
                    self.each(EditKind::Typing, |_, c| CursorEdit::insert(c.range(), unit))
                }
            }
            _ => false,
        }
    }

    fn move_all(&mut self, to: usize, extend: bool) {
        let p = self.view.sel.primary();
        let mut c = p;
        c.move_to(to, extend);
        c.goal = None;
        self.view.sel = Selections::single(c);
    }

    fn vertical(&mut self, delta: isize, extend: bool) {
        let wrap = self.wrap_cols;
        let map = self.view.display_map(&self.doc.buffer, wrap).clone();
        let buf = &self.doc.buffer;
        self.view.sel.update(|c| {
            if !extend && !c.is_empty() {
                // Collapse to the side we're moving towards first.
                let to = if delta > 0 { c.end() } else { c.start() };
                c.move_to(to, false);
            }
            let (to, goal) = view::move_vertically(buf, &map, wrap, c.head, c.goal, delta);
            c.move_to(to, extend);
            c.goal = Some(goal);
        });
    }

    /// Moving left/right into a fold jumps over it.
    fn skip_folds(&mut self, right: bool) {
        if self.view.folds().is_empty() {
            return;
        }
        let buf = &self.doc.buffer;
        let folds = self.view.folds().to_vec();
        self.view.sel.update(|c| {
            let line = buf.char_to_line(c.head);
            if let Some(f) = folds
                .iter()
                .filter(|f| f.hides(line))
                .min_by_key(|f| f.start)
            {
                let header_end = buf.line_start(f.start) + buf.line_len(f.start);
                let to = if right && f.end + 1 < buf.line_count() {
                    buf.line_start(f.end + 1)
                } else {
                    header_end
                };
                let extend = c.anchor != c.head;
                c.move_to(to, extend);
            }
        });
    }
}

fn line_end(buf: &super::Buffer, at: usize) -> usize {
    let line = buf.char_to_line(at);
    buf.line_start(line) + buf.line_len(line)
}

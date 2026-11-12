//! Small vector icons painted with egui shapes, so they render the same on
//! every platform without an icon font.

use egui::epaint::{CubicBezierShape, PathShape, PathStroke, QuadraticBezierShape};
use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Files,
    Branch,
    PullRequest,
    Refresh,
    Check,
    Plus,
    Minus,
    Discard,
    OpenFile,
    More,
    Sync,
    ArrowUp,
    ArrowDown,
    Close,
    Comment,
    Commit,
    Stash,
    External,
    Merge,
    History,
    Search,
    Preview,
}

/// Paints `icon` inside `rect` using `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let o = rect.center() - vec2(s, s) / 2.0;
    // Maps unit coordinates (0..1) into the icon square.
    let p = |x: f32, y: f32| -> Pos2 { o + vec2(x * s, y * s) };
    let w = (s / 12.0).clamp(1.2, 2.0);
    let stroke = Stroke::new(w, color);
    let line = |a: Pos2, b: Pos2| {
        painter.line_segment([a, b], stroke);
    };
    let circle = |c: Pos2, r: f32| {
        painter.circle_stroke(c, r * s, stroke);
    };
    let poly = |pts: Vec<Pos2>| {
        painter.add(PathShape::line(pts, PathStroke::new(w, color)));
    };
    let arc = |c: Pos2, r: f32, from: f32, to: f32| -> Vec<Pos2> {
        let n = 16;
        (0..=n)
            .map(|i| {
                let a = from + (to - from) * i as f32 / n as f32;
                c + vec2(a.cos(), a.sin()) * r * s
            })
            .collect()
    };

    match icon {
        Icon::Files => {
            // Back page.
            poly(vec![p(0.38, 0.14), p(0.82, 0.14), p(0.82, 0.70)]);
            // Front page with folded corner.
            poly(vec![
                p(0.56, 0.28),
                p(0.22, 0.28),
                p(0.22, 0.88),
                p(0.68, 0.88),
                p(0.68, 0.40),
                p(0.56, 0.28),
                p(0.56, 0.40),
                p(0.68, 0.40),
            ]);
        }
        Icon::Branch => {
            circle(p(0.30, 0.20), 0.09);
            circle(p(0.30, 0.80), 0.09);
            circle(p(0.72, 0.30), 0.09);
            line(p(0.30, 0.29), p(0.30, 0.71));
            painter.add(QuadraticBezierShape::from_points_stroke(
                [p(0.72, 0.39), p(0.72, 0.62), p(0.31, 0.68)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        }
        Icon::PullRequest => {
            circle(p(0.28, 0.20), 0.09);
            circle(p(0.28, 0.80), 0.09);
            line(p(0.28, 0.29), p(0.28, 0.71));
            circle(p(0.72, 0.80), 0.09);
            poly(vec![
                p(0.72, 0.71),
                p(0.72, 0.34),
                p(0.64, 0.24),
                p(0.48, 0.24),
            ]);
            poly(vec![p(0.56, 0.14), p(0.46, 0.24), p(0.56, 0.34)]);
        }
        Icon::Merge => {
            circle(p(0.28, 0.20), 0.09);
            circle(p(0.28, 0.80), 0.09);
            circle(p(0.74, 0.58), 0.09);
            line(p(0.28, 0.29), p(0.28, 0.71));
            painter.add(QuadraticBezierShape::from_points_stroke(
                [p(0.30, 0.30), p(0.36, 0.56), p(0.65, 0.58)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        }
        Icon::Refresh => {
            let c = p(0.5, 0.52);
            poly(arc(c, 0.30, -1.2, 4.2));
            let end = c + vec2((-1.2f32).cos(), (-1.2f32).sin()) * 0.30 * s;
            poly(vec![
                end + vec2(-0.16, -0.06) * s,
                end,
                end + vec2(-0.02, 0.17) * s,
            ]);
        }
        Icon::Sync => {
            let c = p(0.5, 0.5);
            poly(arc(c, 0.30, 3.6, 5.9));
            poly(arc(c, 0.30, 0.46, 2.76));
            let a = c + vec2(5.9f32.cos(), 5.9f32.sin()) * 0.30 * s;
            poly(vec![
                a + vec2(-0.15, -0.04) * s,
                a,
                a + vec2(0.0, -0.16) * s,
            ]);
            let b = c + vec2(2.76f32.cos(), 2.76f32.sin()) * 0.30 * s;
            poly(vec![b + vec2(0.15, 0.04) * s, b, b + vec2(0.0, 0.16) * s]);
        }
        Icon::Check => poly(vec![p(0.20, 0.52), p(0.42, 0.74), p(0.82, 0.30)]),
        Icon::Plus => {
            line(p(0.5, 0.20), p(0.5, 0.80));
            line(p(0.20, 0.5), p(0.80, 0.5));
        }
        Icon::Minus => line(p(0.20, 0.5), p(0.80, 0.5)),
        Icon::Close => {
            line(p(0.26, 0.26), p(0.74, 0.74));
            line(p(0.74, 0.26), p(0.26, 0.74));
        }
        Icon::Discard => {
            // Counter-clockwise "undo" arrow.
            painter.add(CubicBezierShape::from_points_stroke(
                [p(0.26, 0.42), p(0.50, 0.20), p(0.92, 0.36), p(0.70, 0.80)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
            poly(vec![p(0.22, 0.20), p(0.24, 0.44), p(0.46, 0.46)]);
        }
        Icon::OpenFile => {
            poly(vec![
                p(0.58, 0.14),
                p(0.24, 0.14),
                p(0.24, 0.86),
                p(0.76, 0.86),
                p(0.76, 0.32),
                p(0.58, 0.14),
                p(0.58, 0.32),
                p(0.76, 0.32),
            ]);
        }
        Icon::More => {
            for x in [0.25, 0.5, 0.75] {
                painter.circle_filled(p(x, 0.5), (0.07 * s).max(1.5), color);
            }
        }
        Icon::ArrowUp => {
            line(p(0.5, 0.18), p(0.5, 0.82));
            poly(vec![p(0.26, 0.42), p(0.5, 0.18), p(0.74, 0.42)]);
        }
        Icon::ArrowDown => {
            line(p(0.5, 0.18), p(0.5, 0.82));
            poly(vec![p(0.26, 0.58), p(0.5, 0.82), p(0.74, 0.58)]);
        }
        Icon::Comment => {
            painter.rect_stroke(
                Rect::from_min_max(p(0.16, 0.20), p(0.84, 0.66)),
                (0.10 * s) as u8,
                stroke,
                egui::StrokeKind::Middle,
            );
            poly(vec![p(0.32, 0.66), p(0.30, 0.84), p(0.50, 0.66)]);
        }
        Icon::Commit => {
            circle(p(0.5, 0.5), 0.16);
            line(p(0.08, 0.5), p(0.34, 0.5));
            line(p(0.66, 0.5), p(0.92, 0.5));
        }
        Icon::Stash => {
            poly(vec![
                p(0.16, 0.48),
                p(0.16, 0.82),
                p(0.84, 0.82),
                p(0.84, 0.48),
            ]);
            line(p(0.16, 0.48), p(0.38, 0.48));
            line(p(0.62, 0.48), p(0.84, 0.48));
            line(p(0.5, 0.14), p(0.5, 0.60));
            poly(vec![p(0.36, 0.46), p(0.5, 0.60), p(0.64, 0.46)]);
        }
        Icon::Search => {
            // Magnifying glass.
            circle(p(0.42, 0.42), 0.24);
            line(p(0.59, 0.59), p(0.84, 0.84));
        }
        Icon::Preview => {
            // Eye.
            poly(arc(p(0.5, 1.02), 0.62, -2.47, -0.67));
            poly(arc(p(0.5, -0.02), 0.62, 0.67, 2.47));
            circle(p(0.5, 0.5), 0.12);
        }
        Icon::History => {
            let c = p(0.5, 0.52);
            poly(arc(c, 0.32, -2.6, 3.0));
            poly(vec![p(0.5, 0.32), p(0.5, 0.54), p(0.64, 0.62)]);
            poly(vec![p(0.10, 0.30), p(0.22, 0.38), p(0.30, 0.24)]);
        }
        Icon::External => {
            poly(vec![
                p(0.44, 0.22),
                p(0.20, 0.22),
                p(0.20, 0.80),
                p(0.78, 0.80),
                p(0.78, 0.56),
            ]);
            line(p(0.46, 0.54), p(0.82, 0.18));
            poly(vec![p(0.58, 0.18), p(0.82, 0.18), p(0.82, 0.42)]);
        }
    }
}

/// A frameless icon button with a hover background and tooltip.
pub fn button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    button_sized(ui, icon, tooltip, 22.0)
}

pub fn button_sized(ui: &mut Ui, icon: Icon, tooltip: &str, size: f32) -> Response {
    button_impl(ui, icon, tooltip, size, None)
}

/// Like [`button`], drawn in `tint` (e.g. the accent color while a toggle is on).
pub fn button_tinted(ui: &mut Ui, icon: Icon, tooltip: &str, tint: Option<Color32>) -> Response {
    button_impl(ui, icon, tooltip, 22.0, tint)
}

fn button_impl(
    ui: &mut Ui,
    icon: Icon,
    tooltip: &str,
    size: f32,
    tint: Option<Color32>,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let enabled = ui.is_enabled();
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if response.hovered() && enabled {
            ui.painter()
                .rect_filled(rect, visuals.corner_radius, visuals.bg_fill);
        }
        let color = if enabled {
            tint.unwrap_or(visuals.fg_stroke.color)
        } else {
            ui.visuals().weak_text_color()
        };
        paint(ui.painter(), rect.shrink(size * 0.18), icon, color);
    }
    response.on_hover_text(tooltip)
}

/// Paints an icon inline as a label (no interaction).
pub fn label(ui: &mut Ui, icon: Icon, color: Color32, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect, icon, color);
    }
    response
}

/// A small rounded badge with a number or short text.
pub fn badge(painter: &Painter, anchor: Pos2, text: &str, fill: Color32, text_color: Color32) {
    let font = egui::FontId::proportional(9.5);
    let galley = painter.layout_no_wrap(text.to_owned(), font, text_color);
    let size = vec2((galley.size().x + 7.0).max(15.0), 15.0);
    let rect = Rect::from_center_size(anchor, size);
    painter.rect_filled(rect, 7.5, fill);
    painter.galley(
        pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_color,
    );
}

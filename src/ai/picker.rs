//! The model picker: favorites and providers on the left, a searchable model
//! list in the middle, the reasoning level on the right and (for agents) the
//! access mode along the bottom.

use egui::epaint::{PathShape, PathStroke};
use egui::{Color32, Key, Modifiers, Painter, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use super::provider::{Access, Provider};
use super::thread::ModelChoice;
use crate::theme::UiColors;

const CLAUDE_ORANGE: Color32 = Color32::from_rgb(0xD9, 0x77, 0x57);
const ROW_HEIGHT: f32 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Favorites,
    Provider(Provider),
}

/// Per-picker UI state that lives across frames.
#[derive(Debug, Clone)]
pub struct PickerState {
    search: String,
    tab: Tab,
    show_more: bool,
    focus_search: bool,
}

impl Default for PickerState {
    fn default() -> Self {
        Self {
            search: String::new(),
            tab: Tab::Provider(Provider::Claude),
            show_more: false,
            focus_search: false,
        }
    }
}

/// Lists the picker reads and edits: starred and typed-in models.
pub struct Catalog<'a> {
    pub installed: &'a dyn Fn(Provider) -> bool,
    pub favorites: &'a mut Vec<String>,
    pub custom: &'a mut Vec<String>,
}

pub fn key(provider: Provider, model: &str) -> String {
    format!("{}:{model}", provider_key(provider))
}

fn provider_key(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "claude",
        Provider::Cursor => "cursor",
        Provider::Codex => "codex",
    }
}

fn parse_key(k: &str) -> Option<(Provider, &str)> {
    let (p, m) = k.split_once(':')?;
    let p = Provider::ALL.into_iter().find(|x| provider_key(*x) == p)?;
    Some((p, m))
}

/// A row in the model list.
#[derive(Debug, Clone)]
struct Row {
    provider: Provider,
    id: String,
    name: String,
    /// Not in the catalog: typed in by the user.
    custom: bool,
}

/// The pill that shows the current model and opens the picker. Returns
/// whether anything (model, effort, access, favorites) changed.
pub fn button(
    ui: &mut Ui,
    id_salt: &str,
    choice: &mut ModelChoice,
    access: Option<&mut Access>,
    state: &mut PickerState,
    catalog: Catalog<'_>,
    colors: &UiColors,
) -> bool {
    let label = choice.label();
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label, font, colors.text.0);
    let size = vec2(galley.size().x + 40.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() {
        colors.widget_hover.0
    } else {
        colors.widget.0
    };
    ui.painter().rect_filled(rect, 13.0, fill);
    let icon = Rect::from_center_size(pos2(rect.left() + 17.0, rect.center().y), vec2(16.0, 16.0));
    provider_icon(ui.painter(), icon, choice.provider, colors);
    ui.painter().galley(
        pos2(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0),
        galley,
        colors.text.0,
    );
    let response = response.on_hover_text(format!(
        "{} via the {} CLI",
        choice.provider.model_name(&choice.model),
        choice.provider.name()
    ));
    if response.clicked() {
        state.tab = Tab::Provider(choice.provider);
        state.search.clear();
        state.show_more = false;
        state.focus_search = true;
    }

    let mut changed = false;
    let debug_open = std::env::var("FORGE_DEBUG_PICKER").is_ok_and(|v| v == id_salt);
    let mut popup = egui::Popup::menu(&response)
        .id(egui::Id::unique(("model_picker", id_salt)))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP_START)
        .gap(6.0);
    if debug_open {
        popup = popup.open(true);
    }
    popup
        .show(|ui| {
            changed = contents(ui, choice, access, state, catalog, colors);
        });
    changed
}

// Fixed layout: a popup has no size of its own, so anything that fills
// "available" space would grow it every frame.
const RAIL_W: f32 = 40.0;
const LIST_W: f32 = 330.0;
const REASON_W: f32 = 180.0;
const GAP: f32 = 14.0;
const BODY_H: f32 = 360.0;

fn contents(
    ui: &mut Ui,
    choice: &mut ModelChoice,
    access: Option<&mut Access>,
    state: &mut PickerState,
    mut catalog: Catalog<'_>,
    colors: &UiColors,
) -> bool {
    let mut changed = false;
    let width = RAIL_W + LIST_W + REASON_W + 2.0 * GAP;
    ui.set_width(width);
    ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
    let (body, _) = ui.allocate_exact_size(vec2(width, BODY_H), Sense::hover());
    let column = |x: f32, w: f32| Rect::from_min_size(pos2(x, body.top()), vec2(w, BODY_H));
    let rail_rect = column(body.left(), RAIL_W);
    let list_rect = column(rail_rect.right() + GAP, LIST_W);
    let reason_rect = column(list_rect.right() + GAP, REASON_W);
    let divider = Stroke::new(1.0, colors.border.0);
    ui.painter()
        .vline(rail_rect.right() + GAP / 2.0, body.y_range(), divider);
    ui.painter()
        .vline(list_rect.right() + GAP / 2.0, body.y_range(), divider);
    let mut child = |rect: Rect| {
        let mut c = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        c.set_clip_rect(rect.intersect(ui.clip_rect()));
        c
    };
    rail(&mut child(rail_rect), state, &catalog, colors);
    changed |= model_list(&mut child(list_rect), choice, state, &mut catalog, colors);
    changed |= reasoning(&mut child(reason_rect), choice, colors);

    if let Some(access) = access {
        ui.add_space(6.0);
        let y = ui.cursor().top();
        ui.painter().hline(body.x_range(), y, divider);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("Access").small().color(colors.text_muted.0));
            ui.add_space(6.0);
            for a in Access::ALL {
                if ui
                    .selectable_label(*access == a, RichText::new(a.label()).small())
                    .on_hover_text(a.describe())
                    .clicked()
                    && *access != a
                {
                    *access = a;
                    changed = true;
                }
            }
        });
    }
    changed
}

fn rail(ui: &mut Ui, state: &mut PickerState, catalog: &Catalog<'_>, colors: &UiColors) {
    {
        let tabs = std::iter::once(Tab::Favorites).chain(Provider::ALL.map(Tab::Provider));
        for (i, tab) in tabs.enumerate() {
            if i == 1 {
                ui.add_space(4.0);
                let y = ui.cursor().top();
                ui.painter().hline(
                    ui.max_rect().x_range().shrink(6.0),
                    y,
                    Stroke::new(1.0, colors.border.0),
                );
                ui.add_space(6.0);
            }
            let (rect, response) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
            let active = state.tab == tab && state.search.is_empty();
            if active || response.hovered() {
                ui.painter()
                    .rect_filled(rect.shrink(2.0), 6.0, colors.widget.0);
            }
            if active {
                ui.painter().vline(
                    rect.right() + 1.0,
                    rect.y_range().shrink(8.0),
                    Stroke::new(2.5, colors.accent.0),
                );
            }
            let icon_rect = Rect::from_center_size(rect.center(), vec2(20.0, 20.0));
            let tip = match tab {
                Tab::Favorites => {
                    star(ui.painter(), icon_rect, colors.text.0, true);
                    "Favorites".to_owned()
                }
                Tab::Provider(p) => {
                    provider_icon(ui.painter(), icon_rect, p, colors);
                    if !(catalog.installed)(p) {
                        // Dim providers whose CLI isn't installed.
                        ui.painter().rect_filled(
                            rect.shrink(2.0),
                            6.0,
                            colors.panel.0.gamma_multiply(0.6),
                        );
                    }
                    if (catalog.installed)(p) {
                        p.name().to_owned()
                    } else {
                        format!("{} (CLI not installed)", p.name())
                    }
                }
            };
            if response.on_hover_text(tip).clicked() {
                state.tab = tab;
                state.search.clear();
                state.show_more = false;
            }
        }
    }
}

fn rows_for(provider: Provider, show_more: bool, custom: &[String]) -> Vec<Row> {
    let mut rows: Vec<Row> = provider
        .models()
        .iter()
        .filter(|m| show_more || !m.more)
        .map(|m| Row {
            provider,
            id: m.id.to_owned(),
            name: m.name.to_owned(),
            custom: false,
        })
        .collect();
    for k in custom {
        if let Some((p, m)) = parse_key(k)
            && p == provider
            && !provider.models().iter().any(|x| x.id == m)
        {
            rows.push(Row {
                provider,
                id: m.to_owned(),
                name: m.to_owned(),
                custom: true,
            });
        }
    }
    rows
}

fn model_list(
    ui: &mut Ui,
    choice: &mut ModelChoice,
    state: &mut PickerState,
    catalog: &mut Catalog<'_>,
    colors: &UiColors,
) -> bool {
    let mut changed = false;
    // Search box with an accent underline.
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let icon = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover()).0;
        crate::ui::icons::paint(
            ui.painter(),
            icon,
            crate::ui::icons::Icon::Search,
            colors.text_muted.0,
        );
        ui.add_space(6.0);
        let edit = ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .hint_text("Search models…")
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY),
        );
        if state.focus_search {
            edit.request_focus();
            state.focus_search = false;
        }
    });
    ui.add_space(4.0);
    let y = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        y,
        Stroke::new(1.5, colors.accent.0),
    );
    ui.add_space(8.0);

    let query = state.search.trim().to_lowercase();
    let mut rows: Vec<Row> = if !query.is_empty() {
        Provider::ALL
            .into_iter()
            .flat_map(|p| rows_for(p, true, catalog.custom))
            .filter(|r| {
                r.name.to_lowercase().contains(&query)
                    || r.id.to_lowercase().contains(&query)
                    || r.provider.name().to_lowercase().contains(&query)
            })
            .collect()
    } else {
        match state.tab {
            Tab::Favorites => catalog
                .favorites
                .iter()
                .filter_map(|k| parse_key(k))
                .map(|(p, m)| Row {
                    provider: p,
                    id: m.to_owned(),
                    name: p.model_name(m),
                    custom: !p.models().iter().any(|x| x.id == m),
                })
                .collect(),
            Tab::Provider(p) => rows_for(p, state.show_more, catalog.custom),
        }
    };
    // Typing a model id the list doesn't know offers to use it as-is.
    let custom_target = match state.tab {
        Tab::Provider(p) => p,
        Tab::Favorites => choice.provider,
    };
    let raw = state.search.trim().to_owned();
    if !raw.is_empty() && !raw.contains(char::is_whitespace) && !rows.iter().any(|r| r.id == raw) {
        rows.push(Row {
            provider: custom_target,
            id: raw.clone(),
            name: format!("Use “{raw}” with {}", custom_target.name()),
            custom: true,
        });
    }

    if let Tab::Provider(p) = state.tab
        && query.is_empty()
        && !(catalog.installed)(p)
    {
        ui.label(
            RichText::new(format!(
                "The {} CLI isn't installed. Install it with:",
                p.name()
            ))
            .small()
            .color(colors.warning.0),
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new(p.install_hint()).small().monospace());
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(p.install_hint().to_owned());
            }
        });
        ui.add_space(6.0);
    }

    // ⌘1–⌘9 pick the first rows.
    let picked_by_key = ui.input_mut(|i| {
        (0..rows.len().min(9)).find(|n| {
            let key = Key::from_name(&(n + 1).to_string()).unwrap_or(Key::Num1);
            i.consume_key(Modifiers::COMMAND, key)
        })
    });

    let mut select: Option<Row> = picked_by_key.map(|n| rows[n].clone());
    let mut toggle_fav: Option<String> = None;
    // Fill the rest of the column, and no more.
    let list_height = (ui.available_height() - 4.0).max(80.0);
    egui::ScrollArea::vertical()
        .id_salt("model_rows")
        .max_height(list_height)
        .min_scrolled_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if rows.is_empty() {
                ui.add_space(8.0);
                let text = if state.tab == Tab::Favorites && query.is_empty() {
                    "Star a model to pin it here"
                } else {
                    "No models match"
                };
                ui.label(RichText::new(text).color(colors.text_muted.0));
            }
            for (i, row) in rows.iter().enumerate() {
                let fav_key = key(row.provider, &row.id);
                let is_fav = catalog.favorites.contains(&fav_key);
                let selected = choice.provider == row.provider && choice.model == row.id;
                let installed = (catalog.installed)(row.provider);
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
                let hovered = response.hovered();
                if selected || hovered {
                    ui.painter().rect_filled(rect, 6.0, colors.widget.0);
                }
                let text_color = if installed {
                    colors.text.0
                } else {
                    colors.text_muted.0
                };
                let icon = Rect::from_center_size(
                    pos2(rect.left() + 16.0, rect.center().y),
                    vec2(18.0, 18.0),
                );
                provider_icon(ui.painter(), icon, row.provider, colors);
                let name = ui.painter().layout_no_wrap(
                    row.name.clone(),
                    egui::TextStyle::Body.resolve(ui.style()),
                    text_color,
                );
                let name_w = name.size().x;
                let name_pos = pos2(rect.left() + 34.0, rect.center().y - name.size().y / 2.0);
                ui.painter().galley(name_pos, name, text_color);
                if selected {
                    let c = Rect::from_center_size(
                        pos2(name_pos.x + name_w + 14.0, rect.center().y),
                        vec2(14.0, 14.0),
                    );
                    crate::ui::icons::paint(
                        ui.painter(),
                        c,
                        crate::ui::icons::Icon::Check,
                        text_color,
                    );
                }
                // Shortcut chip, then the star to its left.
                let mut right = rect.right() - 6.0;
                if i < 9 {
                    let chip = ui.painter().layout_no_wrap(
                        format!("⌘{}", i + 1),
                        egui::FontId::proportional(11.0),
                        colors.text_muted.0,
                    );
                    let r = Rect::from_min_size(
                        pos2(right - chip.size().x - 10.0, rect.center().y - 10.0),
                        vec2(chip.size().x + 10.0, 20.0),
                    );
                    ui.painter().rect_filled(r, 4.0, colors.widget_hover.0);
                    ui.painter()
                        .galley(r.center() - chip.size() / 2.0, chip, colors.text_muted.0);
                    right = r.left() - 8.0;
                }
                let star_rect =
                    Rect::from_center_size(pos2(right - 9.0, rect.center().y), vec2(16.0, 16.0));
                if is_fav || hovered {
                    let c = if is_fav {
                        colors.accent.0
                    } else {
                        colors.text_muted.0
                    };
                    star(ui.painter(), star_rect, c, is_fav);
                }
                let response = if installed {
                    response
                } else {
                    response.on_hover_text(format!(
                        "Install the {} CLI to use this model",
                        row.provider.name()
                    ))
                };
                if response.clicked() {
                    let on_star = response
                        .interact_pointer_pos()
                        .is_some_and(|p| star_rect.expand(4.0).contains(p));
                    if on_star && !row.name.starts_with("Use “") {
                        toggle_fav = Some(fav_key);
                    } else if installed {
                        select = Some(row.clone());
                    }
                }
            }
            if state.tab != Tab::Favorites
                && query.is_empty()
                && let Tab::Provider(p) = state.tab
                && p.models().iter().any(|m| m.more)
            {
                ui.add_space(4.0);
                let text = if state.show_more {
                    "Show less"
                } else {
                    "Show more"
                };
                if ui
                    .add(
                        egui::Label::new(RichText::new(text).color(colors.text_muted.0))
                            .sense(Sense::click()),
                    )
                    .clicked()
                {
                    state.show_more = !state.show_more;
                }
            }
        });

    if let Some(k) = toggle_fav {
        if let Some(i) = catalog.favorites.iter().position(|f| *f == k) {
            catalog.favorites.remove(i);
        } else {
            catalog.favorites.push(k);
        }
        changed = true;
    }
    if let Some(row) = select
        && (catalog.installed)(row.provider)
    {
        if row.custom {
            let k = key(row.provider, &row.id);
            if !catalog.custom.contains(&k) {
                catalog.custom.push(k);
            }
            state.search.clear();
        }
        if choice.provider != row.provider {
            let keep = row
                .provider
                .efforts()
                .iter()
                .any(|(v, _)| *v == choice.effort);
            if !keep {
                choice.effort = row.provider.default_effort().to_owned();
            }
        }
        choice.provider = row.provider;
        choice.model = row.id;
        state.tab = Tab::Provider(row.provider);
        changed = true;
    }
    changed
}

fn reasoning(ui: &mut Ui, choice: &mut ModelChoice, colors: &UiColors) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(RichText::new("Reasoning").color(colors.text.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let name = choice.provider.model_name(&choice.model);
            let short = name.strip_prefix("Claude ").unwrap_or(&name);
            ui.label(RichText::new(short).small().color(colors.text_muted.0));
        });
    });
    ui.add_space(4.0);
    let y = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        y,
        Stroke::new(1.0, colors.border.0),
    );
    ui.add_space(8.0);
    let efforts = choice.provider.efforts();
    if efforts.is_empty() {
        ui.label(
            RichText::new(format!(
                "{} picks reasoning per model (e.g. a “-high” model name).",
                choice.provider.name()
            ))
            .small()
            .color(colors.text_muted.0),
        );
        return false;
    }
    for (value, label) in efforts {
        let selected = choice.effort == *value;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, 6.0, colors.widget.0);
        }
        ui.painter().text(
            pos2(rect.left() + 8.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            *label,
            egui::TextStyle::Body.resolve(ui.style()),
            colors.text.0,
        );
        if selected {
            let c = Rect::from_center_size(
                pos2(rect.right() - 14.0, rect.center().y),
                vec2(14.0, 14.0),
            );
            crate::ui::icons::paint(
                ui.painter(),
                c,
                crate::ui::icons::Icon::Check,
                colors.text.0,
            );
        }
        if response.clicked() && !selected {
            choice.effort = (*value).to_owned();
            changed = true;
        }
    }
    changed
}

// ---- icons -----------------------------------------------------------------

/// The provider's color, used to mark its replies.
pub fn provider_color(provider: Provider, colors: &UiColors) -> Color32 {
    match provider {
        Provider::Claude => CLAUDE_ORANGE,
        Provider::Cursor | Provider::Codex => colors.accent.0,
    }
}

/// The provider's mark, drawn with shapes so no image assets are needed.
pub fn provider_icon(painter: &Painter, rect: Rect, provider: Provider, colors: &UiColors) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    match provider {
        Provider::Claude => {
            // A starburst of rays.
            let n = 12;
            for i in 0..n {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                let len = if i % 2 == 0 { 1.0 } else { 0.78 };
                let dir = vec2(a.cos(), a.sin());
                painter.line_segment(
                    [c + dir * r * 0.18, c + dir * r * len],
                    Stroke::new((r / 5.0).clamp(1.2, 2.2), CLAUDE_ORANGE),
                );
            }
        }
        Provider::Cursor => {
            // A hexagonal cube.
            let color = colors.text.0;
            let pts: Vec<Pos2> = (0..6)
                .map(|i| {
                    let a = (i as f32 * 60.0 - 90.0).to_radians();
                    c + vec2(a.cos(), a.sin()) * r * 0.92
                })
                .collect();
            painter.add(PathShape::closed_line(
                pts.clone(),
                PathStroke::new(1.4, color),
            ));
            painter.line_segment([c, pts[0]], Stroke::new(1.4, color));
            painter.line_segment([c, pts[2]], Stroke::new(1.4, color));
            painter.line_segment([c, pts[4]], Stroke::new(1.4, color));
        }
        Provider::Codex => {
            // A terminal prompt in a rounded square.
            let color = colors.text.0;
            let sq = Rect::from_center_size(c, vec2(r * 1.8, r * 1.8));
            painter.rect_stroke(
                sq,
                r * 0.4,
                Stroke::new(1.4, color),
                egui::StrokeKind::Inside,
            );
            let p = |x: f32, y: f32| pos2(sq.left() + x * sq.width(), sq.top() + y * sq.height());
            painter.add(PathShape::line(
                vec![p(0.26, 0.32), p(0.46, 0.50), p(0.26, 0.68)],
                PathStroke::new(1.4, color),
            ));
            painter.line_segment([p(0.54, 0.68), p(0.76, 0.68)], Stroke::new(1.4, color));
        }
    }
}

/// A five-pointed star, filled or outlined.
fn star(painter: &Painter, rect: Rect, color: Color32, filled: bool) {
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    let pts: Vec<Pos2> = (0..10)
        .map(|i| {
            let a = (i as f32 * 36.0 - 90.0).to_radians();
            let rad = if i % 2 == 0 { r } else { r * 0.45 };
            c + vec2(a.cos(), a.sin()) * rad
        })
        .collect();
    if filled {
        // A star isn't convex; fill it as a fan of triangles from the center.
        for i in 0..10 {
            let a = pts[i];
            let b = pts[(i + 1) % 10];
            painter.add(egui::Shape::convex_polygon(
                vec![c, a, b],
                color,
                Stroke::NONE,
            ));
        }
    } else {
        painter.add(PathShape::closed_line(pts, PathStroke::new(1.3, color)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let k = key(Provider::Codex, "gpt-5.1-codex");
        assert_eq!(k, "codex:gpt-5.1-codex");
        assert_eq!(parse_key(&k), Some((Provider::Codex, "gpt-5.1-codex")));
        assert_eq!(parse_key("nope:x"), None);
    }

    #[test]
    fn custom_models_are_listed_once() {
        let custom = vec!["cursor:sonnet-4.5".to_owned(), "cursor:auto".to_owned()];
        let rows = rows_for(Provider::Cursor, false, &custom);
        assert_eq!(rows.iter().filter(|r| r.id == "auto").count(), 1);
        assert!(rows.iter().any(|r| r.id == "sonnet-4.5" && r.custom));
    }
}

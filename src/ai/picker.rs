//! The model picker: one popover with a filter box, provider chips, models
//! grouped by provider, an effort meter and (for agents) an access switch.

use egui::epaint::{PathShape, PathStroke};
use egui::{Color32, Key, Modifiers, Painter, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use super::provider::{Access, Provider};
use super::thread::ModelChoice;
use crate::theme::UiColors;

const CLAUDE_ORANGE: Color32 = Color32::from_rgb(0xD9, 0x77, 0x57);
/// The popover's fixed size: a popup has none of its own, so anything that
/// fills "available" space would grow it every frame.
const WIDTH: f32 = 380.0;
const LIST_HEIGHT: f32 = 270.0;
const ROW_HEIGHT: f32 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Starred,
    Provider(Provider),
}

/// Per-picker UI state that lives across frames.
#[derive(Debug, Clone)]
pub struct PickerState {
    search: String,
    tab: Tab,
    /// Providers whose extra models are listed.
    expanded: Vec<Provider>,
    /// Keyboard highlight in the visible rows.
    cursor: Option<usize>,
    focus_search: bool,
}

impl Default for PickerState {
    fn default() -> Self {
        Self {
            search: String::new(),
            tab: Tab::All,
            expanded: Vec::new(),
            cursor: None,
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

fn rows_for(provider: Provider, show_more: bool, custom: &[String]) -> Vec<Row> {
    let models = provider.models();
    let mut rows: Vec<Row> = models
        .iter()
        .filter(|m| show_more || !m.more)
        .map(|m| Row {
            provider,
            id: m.id.clone(),
            name: m.name.clone(),
            custom: false,
        })
        .collect();
    for k in custom {
        if let Some((p, m)) = parse_key(k)
            && p == provider
            && !models.iter().any(|x| x.id == m)
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

/// A model's name without the provider prefix, for rows under its heading.
fn short_name(name: &str) -> &str {
    name.strip_prefix("Claude ").unwrap_or(name)
}

/// The button that shows the current model and opens the picker. Returns
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
    let name = choice.provider.model_name(&choice.model);
    let name = short_name(&name).to_owned();
    let effort = choice.provider.effort_label(&choice.model, &choice.effort);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let name_g = ui
        .painter()
        .layout_no_wrap(name, font.clone(), colors.text.0);
    let effort_g = effort.map(|e| {
        ui.painter()
            .layout_no_wrap(format!("· {e}"), font, colors.text_muted.0)
    });
    let effort_w = effort_g.as_ref().map_or(0.0, |g| g.size().x + 5.0);
    let size = vec2(26.0 + name_g.size().x + effort_w + 22.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Id::unique(("model_picker", id_salt)));
    let border = if open || response.hovered() {
        colors.accent.0
    } else {
        colors.border.0
    };
    ui.painter()
        .rect_filled(rect, 5.0, colors.panel.0.gamma_multiply(0.6));
    ui.painter().rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    let icon = Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(14.0, 14.0));
    provider_icon(ui.painter(), icon, choice.provider, colors);
    let mut x = rect.left() + 26.0;
    let y = rect.center().y - name_g.size().y / 2.0;
    let name_w = name_g.size().x;
    ui.painter().galley(pos2(x, y), name_g, colors.text.0);
    x += name_w + 5.0;
    if let Some(g) = effort_g {
        ui.painter().galley(pos2(x, y), g, colors.text_muted.0);
    }
    chevron(
        ui.painter(),
        pos2(rect.right() - 11.0, rect.center().y),
        colors.text_muted.0,
        open,
    );
    let response = response.on_hover_text(format!(
        "{} via the {} CLI",
        choice.provider.model_name(&choice.model),
        choice.provider.name()
    ));
    if response.clicked() {
        state.tab = Tab::All;
        state.search.clear();
        state.cursor = None;
        state.focus_search = true;
    }

    let mut changed = false;
    egui::Popup::menu(&response)
        .id(egui::Id::unique(("model_picker", id_salt)))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(egui::RectAlign::TOP_START)
        .gap(6.0)
        .show(|ui| {
            changed = contents(ui, choice, access, state, catalog, colors);
        });
    changed
}

fn contents(
    ui: &mut Ui,
    choice: &mut ModelChoice,
    access: Option<&mut Access>,
    state: &mut PickerState,
    catalog: Catalog<'_>,
    colors: &UiColors,
) -> bool {
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
    let mut changed = false;

    // Filter box.
    egui::Frame::new()
        .fill(colors.panel.0)
        .stroke(Stroke::new(1.0, colors.border.0))
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                crate::ui::icons::paint(
                    ui.painter(),
                    r,
                    crate::ui::icons::Icon::Search,
                    colors.text_muted.0,
                );
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut state.search)
                        .hint_text("Filter models, or type a model id")
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                );
                if state.focus_search {
                    edit.request_focus();
                    state.focus_search = false;
                }
                if edit.changed() {
                    state.cursor = None;
                }
            });
        });

    // Provider chips.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let tabs = [Tab::All, Tab::Starred]
            .into_iter()
            .chain(Provider::ALL.map(Tab::Provider));
        for tab in tabs {
            if chip(ui, tab, state.tab == tab, &catalog, colors).clicked() {
                state.tab = tab;
                state.cursor = None;
            }
        }
    });
    ui.add_space(2.0);
    let y = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        y,
        Stroke::new(1.0, colors.border.0),
    );
    ui.add_space(4.0);

    // Which rows to show, grouped by provider.
    let query = state.search.trim().to_lowercase();
    let providers: Vec<Provider> = match state.tab {
        Tab::Provider(p) => vec![p],
        _ => Provider::ALL.to_vec(),
    };
    let mut groups: Vec<(Provider, Vec<Row>, usize)> = Vec::new();
    for p in providers {
        let expanded = state.expanded.contains(&p) || !query.is_empty();
        let all = rows_for(p, true, catalog.custom);
        let mut rows: Vec<Row> = rows_for(p, expanded, catalog.custom);
        if state.tab == Tab::Starred {
            rows = all
                .iter()
                .filter(|r| catalog.favorites.contains(&key(r.provider, &r.id)))
                .cloned()
                .collect();
        }
        if !query.is_empty() {
            rows.retain(|r| {
                r.name.to_lowercase().contains(&query)
                    || r.id.to_lowercase().contains(&query)
                    || p.name().to_lowercase().contains(&query)
            });
        }
        let hidden = if expanded || state.tab == Tab::Starred {
            0
        } else {
            all.len() - rows.len()
        };
        if !rows.is_empty() || (hidden > 0 && query.is_empty()) {
            groups.push((p, rows, hidden));
        }
    }
    // A model id the list doesn't know can be used as typed.
    let raw = state.search.trim().to_owned();
    let custom_target = match state.tab {
        Tab::Provider(p) => p,
        _ => choice.provider,
    };
    let offer_custom = !raw.is_empty()
        && !raw.contains(char::is_whitespace)
        && !groups
            .iter()
            .any(|(_, rows, _)| rows.iter().any(|r| r.id == raw));
    let mut flat: Vec<Row> = groups
        .iter()
        .flat_map(|(_, rows, _)| rows.clone())
        .collect();
    if offer_custom {
        flat.push(Row {
            provider: custom_target,
            id: raw.clone(),
            name: format!("Use “{raw}” with {}", custom_target.name()),
            custom: true,
        });
    }

    // Keyboard: ↑/↓ move, Enter picks.
    let (down, up, enter) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Enter),
        )
    });
    if !flat.is_empty() {
        let last = flat.len() - 1;
        if down {
            state.cursor = Some(state.cursor.map_or(0, |c| (c + 1).min(last)));
        }
        if up {
            state.cursor = Some(state.cursor.map_or(0, |c| c.saturating_sub(1)));
        }
        state.cursor = state.cursor.map(|c| c.min(last));
    }
    let mut select: Option<Row> = None;
    if enter {
        let pick = state.cursor.unwrap_or(0);
        select = flat.get(pick).cloned();
    }

    let mut toggle_fav: Option<String> = None;
    let mut toggle_more: Option<Provider> = None;
    egui::ScrollArea::vertical()
        .id_salt("model_rows")
        .max_height(LIST_HEIGHT)
        .min_scrolled_height(LIST_HEIGHT)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if flat.is_empty() {
                ui.add_space(10.0);
                let text = if state.tab == Tab::Starred && query.is_empty() {
                    "No starred models yet. Hover a model and click ☆ to star it."
                } else {
                    "No models match"
                };
                ui.label(RichText::new(text).small().color(colors.text_muted.0));
            }
            let mut index = 0;
            for (p, rows, hidden) in &groups {
                group_header(ui, *p, &catalog, colors);
                for row in rows {
                    let fav_key = key(row.provider, &row.id);
                    let is_fav = catalog.favorites.contains(&fav_key);
                    let selected = choice.provider == row.provider && choice.model == row.id;
                    let highlighted = state.cursor == Some(index);
                    let installed = (catalog.installed)(row.provider);
                    match model_row(ui, row, selected, highlighted, is_fav, installed, colors) {
                        RowClick::Select => select = Some(row.clone()),
                        RowClick::Star => toggle_fav = Some(fav_key),
                        RowClick::None => {}
                    }
                    index += 1;
                }
                if *hidden > 0 || state.expanded.contains(p) && query.is_empty() {
                    let text = if *hidden > 0 {
                        format!("+ {hidden} more")
                    } else {
                        "− fewer".to_owned()
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        if ui
                            .add(
                                egui::Label::new(RichText::new(text).small().color(colors.link.0))
                                    .sense(Sense::click()),
                            )
                            .clicked()
                        {
                            toggle_more = Some(*p);
                        }
                    });
                }
                ui.add_space(4.0);
            }
            if offer_custom {
                let row = flat.last().unwrap().clone();
                let installed = (catalog.installed)(row.provider);
                let highlighted = state.cursor == Some(flat.len() - 1);
                if let RowClick::Select =
                    model_row(ui, &row, false, highlighted, false, installed, colors)
                {
                    select = Some(row);
                }
            }
        });

    if let Some(p) = toggle_more {
        if let Some(i) = state.expanded.iter().position(|x| *x == p) {
            state.expanded.remove(i);
        } else {
            state.expanded.push(p);
        }
    }
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
        // Keep the effort when the new model takes it; Cursor models have
        // none (the level is part of the model id).
        let keep = row
            .provider
            .efforts_for(&row.id)
            .iter()
            .any(|(v, _)| *v == choice.effort);
        if !keep {
            choice.effort = row.provider.default_effort_for(&row.id);
        }
        if choice.provider != row.provider || choice.model != row.id {
            choice.provider = row.provider;
            choice.model = row.id;
            changed = true;
        }
    }

    // Effort and access.
    let y = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        y,
        Stroke::new(1.0, colors.border.0),
    );
    ui.add_space(6.0);
    changed |= effort_meter(ui, choice, colors);
    if let Some(access) = access {
        ui.add_space(2.0);
        changed |= access_switch(ui, access, colors);
    }
    changed
}

/// A provider (or All / Starred) filter chip.
fn chip(
    ui: &mut Ui,
    tab: Tab,
    active: bool,
    catalog: &Catalog<'_>,
    colors: &UiColors,
) -> egui::Response {
    let (label, provider) = match tab {
        Tab::All => ("All".to_owned(), None),
        Tab::Starred => ("★ Starred".to_owned(), None),
        Tab::Provider(p) => (p.name().to_owned(), Some(p)),
    };
    let dim = provider.is_some_and(|p| !(catalog.installed)(p));
    let text_color = if active {
        colors.accent_text.0
    } else if dim {
        colors.text_muted.0
    } else {
        colors.text.0
    };
    let font = egui::FontId::proportional(12.0);
    let g = ui.painter().layout_no_wrap(label, font, text_color);
    let icon_w = if provider.is_some() { 16.0 } else { 0.0 };
    let size = vec2(g.size().x + icon_w + 16.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if active {
        colors.accent.0
    } else if response.hovered() {
        colors.widget_hover.0
    } else {
        colors.widget.0
    };
    ui.painter().rect_filled(rect, 11.0, fill);
    let mut x = rect.left() + 8.0;
    if let Some(p) = provider {
        let r = Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0));
        provider_icon(ui.painter(), r, p, colors);
        x += icon_w;
    }
    ui.painter()
        .galley(pos2(x, rect.center().y - g.size().y / 2.0), g, text_color);
    match provider {
        Some(p) if dim => response.on_hover_text(format!("The {} CLI isn't installed", p.name())),
        _ => response,
    }
}

fn group_header(ui: &mut Ui, p: Provider, catalog: &Catalog<'_>, colors: &UiColors) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(p.name().to_uppercase())
                .small()
                .strong()
                .color(colors.text_muted.0),
        );
        if !(catalog.installed)(p) {
            ui.label(
                RichText::new("not installed")
                    .small()
                    .color(colors.warning.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Label::new(
                            RichText::new("copy install command")
                                .small()
                                .color(colors.link.0),
                        )
                        .sense(Sense::click()),
                    )
                    .on_hover_text(p.install_hint())
                    .clicked()
                {
                    ui.ctx().copy_text(p.install_hint().to_owned());
                }
            });
        }
    });
}

enum RowClick {
    None,
    Select,
    Star,
}

fn model_row(
    ui: &mut Ui,
    row: &Row,
    selected: bool,
    highlighted: bool,
    is_fav: bool,
    installed: bool,
    colors: &UiColors,
) -> RowClick {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let hovered = response.hovered();
    if hovered || highlighted {
        ui.painter().rect_filled(rect, 4.0, colors.widget.0);
    }
    if highlighted {
        ui.painter().rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, colors.accent.0.gamma_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
    }
    // Radio dot.
    let dot = pos2(rect.left() + 12.0, rect.center().y);
    ui.painter().circle_stroke(
        dot,
        5.5,
        Stroke::new(
            1.2,
            if selected {
                colors.accent.0
            } else {
                colors.text_muted.0
            },
        ),
    );
    if selected {
        ui.painter().circle_filled(dot, 3.0, colors.accent.0);
    }
    let text_color = if installed {
        colors.text.0
    } else {
        colors.text_muted.0
    };
    let name = ui.painter().layout_no_wrap(
        short_name(&row.name).to_owned(),
        egui::TextStyle::Body.resolve(ui.style()),
        text_color,
    );
    let name_end = rect.left() + 26.0 + name.size().x;
    ui.painter().galley(
        pos2(rect.left() + 26.0, rect.center().y - name.size().y / 2.0),
        name,
        text_color,
    );
    // Star on the right; the model id just left of it.
    let star_rect =
        Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(13.0, 13.0));
    let is_use_row = row.name.starts_with("Use “");
    if !is_use_row && (is_fav || hovered) {
        star(
            ui.painter(),
            star_rect,
            if is_fav {
                colors.warning.0
            } else {
                colors.text_muted.0
            },
            is_fav,
        );
    }
    let id_text = if row.id.is_empty() {
        "default".to_owned()
    } else if row.custom && !is_use_row {
        format!("{} · custom", row.id)
    } else if is_use_row {
        String::new()
    } else {
        row.id.clone()
    };
    if !id_text.is_empty() {
        let mut job = egui::text::LayoutJob::simple_singleline(
            id_text,
            egui::FontId::monospace(10.5),
            colors.text_muted.0,
        );
        let room = (star_rect.left() - 8.0) - (name_end + 12.0);
        if room > 30.0 {
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            let g = ui.fonts_mut(|f| f.layout_job(job));
            ui.painter().galley(
                pos2(
                    star_rect.left() - 8.0 - g.size().x,
                    rect.center().y - g.size().y / 2.0,
                ),
                g,
                colors.text_muted.0,
            );
        }
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
            .is_some_and(|p| star_rect.expand(5.0).contains(p));
        if on_star && !is_use_row {
            return RowClick::Star;
        }
        if installed {
            return RowClick::Select;
        }
    }
    RowClick::None
}

/// Reasoning effort as a row of bars: more filled bars, more thinking.
fn effort_meter(ui: &mut Ui, choice: &mut ModelChoice, colors: &UiColors) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Effort").small().color(colors.text_muted.0));
        ui.add_space(8.0);
        let efforts = choice.provider.efforts_for(&choice.model);
        if efforts.is_empty() {
            let text = if choice.provider == Provider::Cursor {
                "part of the model: pick a -high or -thinking variant above".to_owned()
            } else {
                format!("set by {}", choice.provider.name())
            };
            ui.label(RichText::new(text).small().color(colors.text_muted.0));
            return;
        }
        let current = efforts.iter().position(|(v, _)| *v == choice.effort);
        ui.spacing_mut().item_spacing.x = 3.0;
        for (i, (value, label)) in efforts.iter().enumerate() {
            // Bars step up in height like a signal meter.
            let h = 6.0 + 3.0 * i as f32;
            let (slot, response) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::click());
            let bar = Rect::from_min_max(
                pos2(slot.left() + 2.0, slot.bottom() - h - 2.0),
                pos2(slot.right() - 2.0, slot.bottom() - 2.0),
            );
            let on = current.is_some_and(|c| i <= c);
            let fill = if on {
                colors.accent.0
            } else if response.hovered() {
                colors.accent.0.gamma_multiply(0.45)
            } else {
                colors.border.0
            };
            ui.painter().rect_filled(bar, 2.0, fill);
            if response.on_hover_text(label.as_str()).clicked() && current != Some(i) {
                choice.effort = value.clone();
                changed = true;
            }
        }
        ui.add_space(6.0);
        let label = current.map_or("Default", |c| efforts[c].1.as_str());
        ui.label(RichText::new(label).small().color(colors.text.0));
    });
    changed
}

/// Access as a three-way segmented switch.
fn access_switch(ui: &mut Ui, access: &mut Access, colors: &UiColors) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Access").small().color(colors.text_muted.0));
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.x = 0.0;
        let short = |a: Access| match a {
            Access::Full => "Full",
            Access::Edits => "Edit files",
            Access::ReadOnly => "Read only",
        };
        let n = Access::ALL.len();
        let start = ui.cursor().min;
        for (i, a) in Access::ALL.into_iter().enumerate() {
            let on = *access == a;
            let g = ui.painter().layout_no_wrap(
                short(a).to_owned(),
                egui::FontId::proportional(12.0),
                if on {
                    colors.accent_text.0
                } else {
                    colors.text.0
                },
            );
            let (rect, response) =
                ui.allocate_exact_size(vec2(g.size().x + 18.0, 22.0), Sense::click());
            let r = 5;
            let corners = egui::CornerRadius {
                nw: if i == 0 { r } else { 0 },
                sw: if i == 0 { r } else { 0 },
                ne: if i == n - 1 { r } else { 0 },
                se: if i == n - 1 { r } else { 0 },
            };
            let fill = if on {
                colors.accent.0
            } else if response.hovered() {
                colors.widget_hover.0
            } else {
                colors.widget.0
            };
            ui.painter().rect_filled(rect, corners, fill);
            ui.painter()
                .galley(rect.center() - g.size() / 2.0, g, colors.text.0);
            if response.on_hover_text(a.describe()).clicked() && !on {
                *access = a;
                changed = true;
            }
        }
        // One outline around the whole switch.
        let end = ui.min_rect().right();
        let outline = Rect::from_min_max(start, pos2(end, start.y + 22.0));
        ui.painter().rect_stroke(
            outline,
            5.0,
            Stroke::new(1.0, colors.border.0),
            egui::StrokeKind::Inside,
        );
    });
    changed
}

/// A small down (or, when open, up) chevron.
fn chevron(painter: &Painter, c: Pos2, color: Color32, up: bool) {
    let d = if up { -1.0 } else { 1.0 };
    painter.add(PathShape::line(
        vec![
            c + vec2(-3.5, -1.5 * d),
            c + vec2(0.0, 2.0 * d),
            c + vec2(3.5, -1.5 * d),
        ],
        PathStroke::new(1.3, color),
    ));
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

    /// Draws the picker for several frames with no input: it must keep its
    /// tab and size (a popup that grows every frame never settles).
    #[test]
    fn picker_is_stable_without_input() {
        let ctx = egui::Context::default();
        let colors = crate::theme::load_all(None, &[]).0[0].ui.clone();
        let mut choice = ModelChoice::new(Provider::Claude, "claude-opus-5-5");
        let mut access = Access::Full;
        let mut state = PickerState::default();
        let mut favorites = Vec::new();
        let mut custom = Vec::new();
        let installed = |p: Provider| p == Provider::Claude;
        let mut sizes = Vec::new();
        for _ in 0..6 {
            let mut size = egui::Vec2::ZERO;
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let r = ui.scope(|ui| {
                    contents(
                        ui,
                        &mut choice,
                        Some(&mut access),
                        &mut state,
                        Catalog {
                            installed: &installed,
                            favorites: &mut favorites,
                            custom: &mut custom,
                        },
                        &colors,
                    )
                });
                assert!(!r.inner, "nothing should change without input");
                size = r.response.rect.size();
            });
            out.textures_delta.clear();
            sizes.push(size);
        }
        assert_eq!(state.tab, Tab::All);
        assert!(sizes.windows(2).skip(1).all(|w| w[0] == w[1]), "{sizes:?}");
        assert!(sizes[5].y < 600.0, "{sizes:?}");
        assert_eq!(choice.model, "claude-opus-5-5");
    }

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

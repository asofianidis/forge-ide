//! The History tab (commit graph with filters, also used for a single
//! file's history) and the Interactive Rebase tab.

use egui::epaint::CubicBezierShape;
use egui::{Align2, Color32, FontId, Pos2, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::git::graph::{self, GraphRow};
use crate::git::{
    Commit, LogQuery, RebaseAction, RebaseCommit, RebasePlan, RebaseStep, SourceControl,
};
use crate::task::{self, Pending};
use crate::theme::UiColors;
use crate::ui::fmt;

/// Something a history or rebase tab wants the app to do.
pub enum HistoryAction {
    OpenCommit(String),
    OpenRebase(Option<String>),
    /// Close this tab.
    Close,
}

const PAGE: usize = 300;
const ROW_H: f32 = 26.0;
const LANE_W: f32 = 14.0;

pub struct HistoryPane {
    /// Set for a single file's history.
    pub path: Option<String>,
    query: LogQuery,
    /// Filter text being edited (applied on Enter / focus loss).
    author: String,
    grep: String,
    commits: Vec<Commit>,
    rows: Vec<GraphRow>,
    task: Option<Pending<Result<Vec<Commit>, String>>>,
    error: Option<String>,
    loaded: Option<(LogQuery, u64)>,
    /// Commit a "create tag/branch here" popup is open for.
    naming: Option<(String, bool, String)>,
}

impl HistoryPane {
    pub fn new(path: Option<String>) -> Self {
        Self {
            query: LogQuery {
                path: path.clone(),
                limit: PAGE,
                ..LogQuery::default()
            },
            path,
            author: String::new(),
            grep: String::new(),
            commits: Vec::new(),
            rows: Vec::new(),
            task: None,
            error: None,
            loaded: None,
            naming: None,
        }
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => format!("History: {}", p.rsplit('/').next().unwrap_or(p)),
            None => "History".to_owned(),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        sc: &mut SourceControl,
        colors: &UiColors,
    ) -> Option<HistoryAction> {
        if let Some(result) = task::poll(&mut self.task) {
            match result {
                Ok(commits) => {
                    self.rows = graph::layout(
                        commits
                            .iter()
                            .map(|c| (c.hash.as_str(), c.parents.as_slice())),
                    );
                    self.commits = commits;
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        let wanted = (self.query.clone(), sc.revision);
        if self.task.is_none() && self.loaded.as_ref() != Some(&wanted) {
            self.loaded = Some(wanted);
            self.task = sc.load_history(self.query.clone());
        }

        let mut action = None;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.title()).strong().size(15.0));
                    if let Some(p) = &self.path {
                        ui.label(RichText::new(p).color(colors.text_muted.0));
                    }
                    if self.task.is_some() {
                        ui.spinner();
                    }
                });
                ui.horizontal(|ui| {
                    let mut apply = false;
                    ui.label("Author");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.author)
                            .hint_text("name or email")
                            .desired_width(130.0),
                    );
                    apply |= r.lost_focus();
                    ui.label("Message");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.grep)
                            .hint_text("text in commit messages")
                            .desired_width(180.0),
                    );
                    apply |= r.lost_focus();
                    if ui.checkbox(&mut self.query.all, "All branches").changed() {
                        self.query.limit = PAGE;
                    }
                    if apply && (self.author != self.query.author || self.grep != self.query.grep) {
                        self.query.author = self.author.clone();
                        self.query.grep = self.grep.clone();
                        self.query.limit = PAGE;
                    }
                    if (!self.query.author.is_empty() || !self.query.grep.is_empty())
                        && ui.small_button("Clear filters").clicked()
                    {
                        self.author.clear();
                        self.grep.clear();
                        self.query.author.clear();
                        self.query.grep.clear();
                    }
                    ui.label(
                        RichText::new(format!("{} commits", self.commits.len()))
                            .small()
                            .color(colors.text_muted.0),
                    );
                });
            });
        ui.separator();
        if let Some(e) = &self.error {
            ui.colored_label(colors.error.0, e);
        }
        let lanes = self
            .rows
            .iter()
            .map(GraphRow::width)
            .max()
            .unwrap_or(1)
            .min(12);
        let graph_w = lanes as f32 * LANE_W + 10.0;
        let total = self.commits.len() + usize::from(self.commits.len() >= self.query.limit);
        egui::ScrollArea::vertical()
            .id_salt(("history", &self.path))
            .auto_shrink(false)
            .show_rows(ui, ROW_H, total, |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    if i == self.commits.len() {
                        if ui.button("Load more").clicked() {
                            self.query.limit += PAGE;
                        }
                        continue;
                    }
                    if let Some(a) = self.row(ui, i, graph_w, colors, sc) {
                        action = Some(a);
                    }
                }
            });
        self.naming_popup(ui, sc);
        action
    }

    fn row(
        &mut self,
        ui: &mut Ui,
        i: usize,
        graph_w: f32,
        colors: &UiColors,
        sc: &mut SourceControl,
    ) -> Option<HistoryAction> {
        let commit = &self.commits[i];
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, 0, colors.widget_hover.0.gamma_multiply(0.4));
        }
        if let Some(row) = self.rows.get(i) {
            paint_graph(painter, rect.left_top(), row, colors);
        }
        let mut x = rect.left() + graph_w;
        let y = rect.center().y;
        for r in commit.refs.split(", ").filter(|r| !r.is_empty()).take(4) {
            let r = r.trim_start_matches("HEAD -> ");
            let tag = r.starts_with("tag: ");
            let galley = painter.layout_no_wrap(
                r.trim_start_matches("tag: ").to_owned(),
                FontId::proportional(11.0),
                Color32::WHITE,
            );
            let chip =
                egui::Rect::from_min_size(pos2(x, y - 8.0), vec2(galley.size().x + 10.0, 16.0));
            painter.rect_filled(
                chip,
                8,
                if tag {
                    colors.warning.0.gamma_multiply(0.7)
                } else {
                    colors.accent.0
                },
            );
            painter.galley(
                pos2(x + 5.0, y - galley.size().y / 2.0),
                galley,
                Color32::WHITE,
            );
            x = chip.right() + 4.0;
        }
        let right = rect.right() - 8.0;
        let meta = format!(
            "{}  ·  {}  ·  {}",
            commit.author,
            fmt::relative_time(commit.time),
            commit.short
        );
        let meta_g = painter.layout_no_wrap(meta, FontId::proportional(11.5), colors.text_muted.0);
        let meta_x = right - meta_g.size().x;
        let max_subject = (meta_x - x - 12.0).max(40.0);
        let mut subject = commit.subject.clone();
        let char_w = 7.0;
        if subject.len() as f32 * char_w > max_subject {
            subject = subject
                .chars()
                .take((max_subject / char_w) as usize)
                .collect::<String>()
                + "…";
        }
        painter.text(
            pos2(x, y),
            Align2::LEFT_CENTER,
            subject,
            FontId::proportional(13.0),
            colors.text.0,
        );
        painter.galley(
            pos2(meta_x, y - meta_g.size().y / 2.0),
            meta_g,
            colors.text_muted.0,
        );

        let hash = commit.hash.clone();
        let mut action = None;
        if response.clicked() {
            action = Some(HistoryAction::OpenCommit(hash.clone()));
        }
        response.context_menu(|ui| {
            ui.set_min_width(220.0);
            if ui.button("Open Commit").clicked() {
                action = Some(HistoryAction::OpenCommit(hash.clone()));
                ui.close();
            }
            if ui.button("Copy Hash").clicked() {
                ui.ctx().copy_text(hash.clone());
                ui.close();
            }
            ui.separator();
            if ui.button("Cherry-pick onto Current Branch").clicked() {
                sc.cherry_pick(hash.clone());
                ui.close();
            }
            if ui
                .button("Revert Commit")
                .on_hover_text("Create a new commit undoing this one")
                .clicked()
            {
                sc.revert_commit(hash.clone());
                ui.close();
            }
            if ui
                .button("Interactive Rebase from Here…")
                .on_hover_text("Rewrite the commits after this one")
                .clicked()
            {
                action = Some(HistoryAction::OpenRebase(Some(hash.clone())));
                ui.close();
            }
            ui.separator();
            if ui.button("Create Branch Here…").clicked() {
                self.naming = Some((hash.clone(), false, String::new()));
                ui.close();
            }
            if ui.button("Create Tag Here…").clicked() {
                self.naming = Some((hash.clone(), true, String::new()));
                ui.close();
            }
            if ui.button("Check Out (Detached)").clicked() {
                sc.checkout_detached(hash.clone());
                ui.close();
            }
        });
        action
    }

    fn naming_popup(&mut self, ui: &mut Ui, sc: &mut SourceControl) {
        let Some((hash, tag, name)) = &mut self.naming else {
            return;
        };
        let mut done = None;
        egui::Modal::new(egui::Id::unique("history_name")).show(ui.ctx(), |ui| {
            ui.heading(if *tag { "Create Tag" } else { "Create Branch" });
            ui.label(RichText::new(format!("At {}", &hash[..hash.len().min(10)])).weak());
            let r = ui.add(egui::TextEdit::singleline(name).hint_text(if *tag {
                "v1.2.3"
            } else {
                "feature/name"
            }));
            if name.is_empty() && !r.has_focus() {
                r.request_focus();
            }
            ui.horizontal(|ui| {
                let ok = !name.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("Create")).clicked()
                    || (ok && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        if let Some(create) = done {
            if create {
                let (hash, tag, name) = self.naming.take().expect("open");
                if tag {
                    sc.create_tag(name.trim().to_owned(), String::new(), Some(hash));
                } else {
                    sc.create_branch(name.trim().to_owned(), Some(hash));
                }
            }
            self.naming = None;
        }
    }
}

const LANE_COLORS: usize = 6;

fn lane_color(i: usize, c: &UiColors) -> Color32 {
    match i % LANE_COLORS {
        0 => c.accent.0,
        1 => c.modified.0,
        2 => c.added.0,
        3 => c.warning.0,
        4 => c.link.0,
        _ => c.deleted.0,
    }
}

/// Paints one row of the commit graph with its top-left at `origin`.
fn paint_graph(painter: &egui::Painter, origin: Pos2, row: &GraphRow, colors: &UiColors) {
    let x = |lane: usize| origin.x + 8.0 + lane as f32 * LANE_W;
    let (top, mid, bot) = (origin.y, origin.y + ROW_H / 2.0, origin.y + ROW_H);
    let seg = |from: Pos2, to: Pos2, color: Color32| {
        let stroke = Stroke::new(1.6, color);
        if (from.x - to.x).abs() < 0.5 {
            painter.line_segment([from, to], stroke);
        } else {
            let dy = (to.y - from.y) / 2.0;
            painter.add(CubicBezierShape::from_points_stroke(
                [from, pos2(from.x, from.y + dy), pos2(to.x, to.y - dy), to],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        }
    };
    for &(a, b) in &row.top {
        // Lines joining the commit take its lane's color.
        let color = lane_color(if b == row.col { a.min(b) } else { a }, colors);
        seg(pos2(x(a), top), pos2(x(b), mid), color);
    }
    for &(a, b) in &row.bottom {
        seg(
            pos2(x(a), mid),
            pos2(x(b), bot),
            lane_color(b.max(a), colors),
        );
    }
    let c = pos2(x(row.col), mid);
    let color = lane_color(row.col, colors);
    if row.merge {
        painter.circle(c, 4.0, colors.panel.0, Stroke::new(1.8, color));
    } else {
        painter.circle_filled(c, 4.0, color);
    }
}

/// The Interactive Rebase tab: reorder commits and choose what to do with
/// each, then run the rebase.
pub struct RebasePane {
    base: Option<String>,
    commits: Vec<(RebaseCommit, RebaseAction, String)>,
    task: Option<Pending<RebasePlan>>,
    error: Option<String>,
    started: bool,
}

impl RebasePane {
    pub fn new(base: Option<String>, sc: &SourceControl) -> Self {
        Self {
            task: sc.load_rebase_commits(base),
            base: None,
            commits: Vec::new(),
            error: None,
            started: false,
        }
    }

    pub fn title(&self) -> String {
        "Interactive Rebase".to_owned()
    }

    fn validation(&self) -> Option<&'static str> {
        let kept: Vec<_> = self
            .commits
            .iter()
            .filter(|(_, a, _)| *a != RebaseAction::Drop)
            .collect();
        if kept.is_empty() {
            return Some("Every commit is dropped");
        }
        if matches!(kept[0].1, RebaseAction::Squash | RebaseAction::Fixup) {
            return Some(
                "The first kept commit can't be squashed or fixed up: there is nothing before it to meld into",
            );
        }
        if self
            .commits
            .iter()
            .any(|(_, a, m)| *a == RebaseAction::Reword && m.trim().is_empty())
        {
            return Some("A reworded commit needs a message");
        }
        None
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        sc: &mut SourceControl,
        colors: &UiColors,
    ) -> Option<HistoryAction> {
        if let Some(result) = task::poll(&mut self.task) {
            match result {
                Ok((base, commits)) => {
                    self.base = base;
                    self.commits = commits
                        .into_iter()
                        .map(|c| {
                            let m = c.message.clone();
                            (c, RebaseAction::Pick, m)
                        })
                        .collect();
                }
                Err(e) => self.error = Some(e),
            }
        }
        let mut action = None;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Interactive Rebase").strong().size(15.0));
                let onto = self
                    .base
                    .as_deref()
                    .map_or("the first commit".to_owned(), |b| b[..b.len().min(10)].to_owned());
                ui.label(
                    RichText::new(format!("{} commit(s) after {onto}, oldest first", self.commits.len()))
                        .color(colors.text_muted.0),
                );
                if self.task.is_some() {
                    ui.spinner();
                }
            });
            ui.label(
                RichText::new(
                    "Reorder with the arrows and choose an action for each commit. Squash and fixup meld a \
                     commit into the one above it. Uncommitted changes are stashed and restored.",
                )
                .small()
                .color(colors.text_muted.0),
            );
            ui.horizontal(|ui| {
                let invalid = self.validation();
                let busy = sc.busy().is_some();
                let start = ui
                    .add_enabled(
                        invalid.is_none() && !busy && !self.started && !self.commits.is_empty(),
                        egui::Button::new(RichText::new("Start Rebase").color(colors.accent_text.0)).fill(colors.accent.0),
                    )
                    .on_disabled_hover_text(invalid.unwrap_or("Nothing to rebase"));
                if start.clicked() {
                    let steps = self
                        .commits
                        .iter()
                        .map(|(c, a, m)| RebaseStep {
                            hash: c.hash.clone(),
                            action: *a,
                            message: m.clone(),
                        })
                        .collect();
                    sc.interactive_rebase(self.base.clone(), steps);
                    self.started = true;
                    action = Some(HistoryAction::Close);
                }
                if ui.button("Cancel").clicked() {
                    action = Some(HistoryAction::Close);
                }
                if let Some(msg) = invalid {
                    ui.colored_label(colors.warning.0, msg);
                }
            });
        });
        ui.separator();
        if let Some(e) = &self.error {
            ui.colored_label(colors.error.0, e);
            return action;
        }
        if self.commits.is_empty() && self.task.is_none() {
            ui.label(
                RichText::new("No commits to rebase: the branch has nothing on top of its base.")
                    .weak(),
            );
        }
        let mut swap = None;
        let n = self.commits.len();
        egui::ScrollArea::vertical()
            .id_salt("rebase_list")
            .auto_shrink(false)
            .show(ui, |ui| {
                for (i, (commit, act, message)) in self.commits.iter_mut().enumerate() {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .outer_margin(0)
                        .fill(if *act == RebaseAction::Drop {
                            colors.deleted.0.gamma_multiply(0.08)
                        } else {
                            Color32::TRANSPARENT
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(i > 0, egui::Button::new("↑").small())
                                    .on_hover_text("Move up")
                                    .clicked()
                                {
                                    swap = Some((i, i - 1));
                                }
                                if ui
                                    .add_enabled(i + 1 < n, egui::Button::new("↓").small())
                                    .on_hover_text("Move down")
                                    .clicked()
                                {
                                    swap = Some((i, i + 1));
                                }
                                egui::ComboBox::from_id_salt(("rebase_action", &commit.hash))
                                    .width(80.0)
                                    .selected_text(act.label())
                                    .show_ui(ui, |ui| {
                                        for a in RebaseAction::ALL {
                                            ui.selectable_value(act, a, a.label())
                                                .on_hover_text(a.describe());
                                        }
                                    });
                                ui.label(
                                    RichText::new(&commit.short)
                                        .monospace()
                                        .color(colors.text_muted.0),
                                );
                                let subject = RichText::new(&commit.subject);
                                ui.label(if *act == RebaseAction::Drop {
                                    subject.strikethrough().weak()
                                } else {
                                    subject
                                });
                            });
                            if *act == RebaseAction::Reword {
                                ui.add(
                                    egui::TextEdit::multiline(message)
                                        .desired_rows(3)
                                        .desired_width(f32::INFINITY),
                                );
                            }
                        });
                    ui.separator();
                }
            });
        if let Some((a, b)) = swap {
            self.commits.swap(a, b);
        }
        action
    }
}

//! The "Search" sidebar: find text across every file in the workspace.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use egui::text::{LayoutJob, TextFormat};
use egui::{RichText, TextStyle};

use super::{ForgeApp, toggle};
use crate::editor::ops::{self, Selection};
use crate::search::{self, ProjectSearch};

#[derive(Default)]
pub(super) struct SearchPanel {
    pub search: ProjectSearch,
    /// Indices of result files whose lines are hidden.
    collapsed: HashSet<usize>,
    pub focus_query: bool,
}

/// A row of the flattened result list.
enum Row {
    File(usize),
    Line(usize, usize),
}

impl ForgeApp {
    pub(super) fn open_project_search(&mut self, ctx: &egui::Context) {
        self.sidebar_view = crate::ui::activity::SidebarView::Search;
        self.sidebar_open = true;
        self.search.focus_query = true;
        // Seed the query with a single-line selection, like the find bar.
        let sel = self.selection(ctx);
        if let Some(doc) = self.documents.get(self.active)
            && !sel.is_empty()
        {
            let selected: String = doc
                .text
                .chars()
                .skip(sel.start)
                .take(sel.end - sel.start)
                .collect();
            if !selected.contains('\n') && selected != self.search.search.query.pattern {
                self.search.search.query.pattern = selected;
                self.restart_search(ctx);
            }
        }
    }

    fn restart_search(&mut self, ctx: &egui::Context) {
        self.search.collapsed.clear();
        match &self.workspace {
            Some(ws) => {
                let root = ws.root.path.clone();
                self.search.search.start(&root, ctx);
            }
            None => self.search.search.clear(),
        }
    }

    /// Opens `path` and selects the match at `line` / `bytes` (within the line).
    fn open_match(&mut self, ctx: &egui::Context, path: &Path, line: usize, bytes: Range<usize>) {
        self.open_file(path);
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if doc.path.as_deref() != Some(path) {
            return;
        }
        let start = ops::line_start(&doc.text, line);
        let line_text: &str = {
            let byte = doc
                .text
                .char_indices()
                .nth(start)
                .map_or(doc.text.len(), |(b, _)| b);
            let rest = &doc.text[byte..];
            &rest[..rest.find('\n').unwrap_or(rest.len())]
        };
        // The file may have changed since the search ran; clamp to the line.
        let clamp = |b: usize| {
            let mut b = b.min(line_text.len());
            while !line_text.is_char_boundary(b) {
                b -= 1;
            }
            line_text[..b].chars().count()
        };
        let sel = Selection::new(start + clamp(bytes.start), start + clamp(bytes.end));
        self.set_selection(ctx, sel, true);
    }

    pub(super) fn search_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.workspace.is_none() {
            ui.label("Open a folder to search its files.");
            if ui.button("Open Folder…").clicked() {
                self.open_folder_dialog();
            }
            return;
        }

        let mut changed = false;
        let query_id = egui::Id::unique("project_search_query");
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.search.search.query.pattern)
                .id(query_id)
                .hint_text("Search")
                .desired_width(f32::INFINITY),
        );
        if self.search.focus_query {
            self.search.focus_query = false;
            field.request_focus();
        }
        changed |= field.changed();
        ui.horizontal(|ui| {
            let q = &mut self.search.search.query;
            changed |= toggle(ui, &mut q.match_case, "Aa", "Match Case");
            changed |= toggle(ui, &mut q.whole_word, "ab", "Match Whole Word");
            changed |= toggle(ui, &mut q.regex, ".*", "Use Regular Expression");
        });
        if changed {
            self.restart_search(&ctx);
        }

        let muted = self.current_theme().ui.text_muted.0;
        let s = &self.search.search;
        ui.horizontal(|ui| {
            if let Some(err) = &s.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
                return;
            }
            if s.query.pattern.is_empty() {
                return;
            }
            if s.is_running() {
                ui.spinner();
            }
            let matches = s.match_count();
            let files = s.results.len();
            let text = match (matches, s.is_running()) {
                (0, true) => "Searching…".to_owned(),
                (0, false) => "No results".to_owned(),
                (m, _) => format!(
                    "{m} result{} in {files} file{}{}",
                    plural(m),
                    plural(files),
                    if s.truncated {
                        format!(" (stopped at {})", search::MAX_RESULTS)
                    } else {
                        String::new()
                    }
                ),
            };
            ui.label(RichText::new(text).small().color(muted));
        });
        ui.add_space(2.0);

        let rows: Vec<Row> = s
            .results
            .iter()
            .enumerate()
            .flat_map(|(fi, f)| {
                let lines = if self.search.collapsed.contains(&fi) {
                    0
                } else {
                    f.lines.len()
                };
                std::iter::once(Row::File(fi)).chain((0..lines).map(move |li| Row::Line(fi, li)))
            })
            .collect();

        let accent = self.current_theme().ui.accent.0;
        let highlight_bg = accent.gamma_multiply(0.35);
        let font = TextStyle::Body.resolve(ui.style());
        let small = TextStyle::Small.resolve(ui.style());
        let text_color = ui.visuals().text_color();
        let row_height = ui.text_style_height(&TextStyle::Body) + 4.0;
        let mut open: Option<(PathBuf, usize, Range<usize>)> = None;
        let mut toggle_file = None;

        egui::ScrollArea::both()
            .id_salt("search_results")
            .auto_shrink(false)
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                for row in &rows[range] {
                    match *row {
                        Row::File(fi) => {
                            let f = &s.results[fi];
                            let arrow = if self.search.collapsed.contains(&fi) {
                                "⏵"
                            } else {
                                "⏷"
                            };
                            let (dir, name) = match f.display.rsplit_once('/') {
                                Some((d, n)) => (d, n),
                                None => ("", f.display.as_str()),
                            };
                            let mut job = LayoutJob::default();
                            job.append(
                                &format!("{arrow} {name}"),
                                0.0,
                                TextFormat::simple(font.clone(), text_color),
                            );
                            job.append(dir, 8.0, TextFormat::simple(small.clone(), muted));
                            job.append(
                                &format!("  {}", f.lines.len()),
                                0.0,
                                TextFormat::simple(small.clone(), muted),
                            );
                            let r = ui.add_sized(
                                [ui.available_width(), row_height],
                                egui::Button::new(job).frame(false).truncate(),
                            );
                            if r.on_hover_text(&f.display).clicked() {
                                toggle_file = Some(fi);
                            }
                        }
                        Row::Line(fi, li) => {
                            let f = &s.results[fi];
                            let m = &f.lines[li];
                            let mut job = LayoutJob::default();
                            job.append(
                                &format!("{:>5}  ", m.line),
                                12.0,
                                TextFormat::simple(small.clone(), muted),
                            );
                            let mut last = 0;
                            for h in &m.highlights {
                                job.append(
                                    &m.preview[last..h.start],
                                    0.0,
                                    TextFormat::simple(font.clone(), text_color),
                                );
                                let mut hl = TextFormat::simple(font.clone(), text_color);
                                hl.background = highlight_bg;
                                job.append(&m.preview[h.clone()], 0.0, hl);
                                last = h.end;
                            }
                            job.append(
                                &m.preview[last..],
                                0.0,
                                TextFormat::simple(font.clone(), text_color),
                            );
                            let r = ui.add_sized(
                                [ui.available_width(), row_height],
                                egui::Button::new(job).frame(false).truncate(),
                            );
                            if r.clicked() {
                                open = Some((f.path.clone(), m.line, m.first.clone()));
                            }
                        }
                    }
                }
            });

        if let Some(fi) = toggle_file
            && !self.search.collapsed.remove(&fi)
        {
            self.search.collapsed.insert(fi);
        }
        if let Some((path, line, bytes)) = open {
            self.open_match(&ctx, &path, line, bytes);
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

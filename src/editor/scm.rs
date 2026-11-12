//! Source control inside the editor: change markers you can click to peek,
//! revert or stage a change; a blame column; and merge conflict actions.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text_edit::TextEditOutput;
use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use super::conflicts::{Conflict, Resolution};
use super::gutter;
use crate::git::SourceControl;
use crate::git::blame::{Blame, BlameCommit};
use crate::git::line_changes::{self, ChangeHunk, LineChanges, Mark};
use crate::task::{self, Pending};
use crate::theme::UiColors;
use crate::ui::fmt;
use crate::ui::icons::{self, Icon};

/// Width of the blame column, in points.
pub const BLAME_WIDTH: f32 = 230.0;
/// Wait this long after the last edit before re-running blame.
const BLAME_DEBOUNCE: Duration = Duration::from_millis(700);

/// Committed content of a file, for the change markers.
struct Base {
    /// Source control revision the content was loaded at.
    revision: u64,
    task: Option<Pending<Option<String>>>,
    content: Option<String>,
    /// Changes computed for the buffer text with this hash.
    cache: Option<(u64, LineChanges)>,
}

#[derive(Default)]
struct BlameState {
    blame: Option<Blame>,
    task: Option<Pending<Result<Blame, String>>>,
    /// Hash of the text the current/pending blame is for.
    text_hash: Option<u64>,
    revision: u64,
    /// When the buffer last changed (for debouncing).
    changed_at: Option<Instant>,
    error: Option<String>,
}

/// The change shown in the peek popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peek {
    pub path: PathBuf,
    pub hunk: usize,
}

/// What the user did in the editor's source control UI.
pub enum EditorAction {
    /// Undo change `hunk` in the buffer.
    Revert(ChangeHunk),
    /// Stage change `hunk` of the saved file.
    Stage(ChangeHunk),
    /// Show change `idx` (and move the cursor there).
    GoToChange(usize),
    ClosePeek,
    OpenCommit(String),
    Resolve(Conflict, Resolution),
}

#[derive(Default)]
pub struct EditorGit {
    bases: HashMap<PathBuf, Base>,
    blames: HashMap<PathBuf, BlameState>,
    pub blame_on: bool,
    pub peek: Option<Peek>,
}

pub fn text_hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl EditorGit {
    pub fn clear(&mut self) {
        self.bases.clear();
        self.blames.clear();
        self.peek = None;
    }

    /// Change markers for `path`, loading the committed version in the
    /// background as needed.
    pub fn changes(&mut self, sc: &SourceControl, path: &Path, text: &str) -> Option<LineChanges> {
        if !sc.is_ready() {
            return None;
        }
        let rel = sc.rel_path(path)?;
        if sc.status.find(&rel).is_some_and(|e| e.is_untracked()) {
            return None;
        }
        let base = self
            .bases
            .entry(path.to_path_buf())
            .or_insert_with(|| Base {
                revision: u64::MAX,
                task: None,
                content: None,
                cache: None,
            });
        if base.task.is_none() && base.revision != sc.revision {
            base.revision = sc.revision;
            base.task = sc.load_head_content(&rel);
        }
        if let Some(content) = task::poll(&mut base.task) {
            base.content = content;
            base.cache = None;
        }
        let content = base.content.as_deref()?;
        let hash = text_hash(text);
        if base.cache.as_ref().is_none_or(|(h, _)| *h != hash) {
            base.cache = Some((hash, line_changes::compute(content, text)));
        }
        base.cache.as_ref().map(|(_, c)| c.clone())
    }

    /// Blame for `path` when the blame column is on. Re-runs (debounced)
    /// when the buffer or repository changes.
    pub fn blame(&mut self, sc: &SourceControl, path: &Path, text: &str) -> Option<Blame> {
        if !self.blame_on || !sc.is_ready() {
            return None;
        }
        let rel = sc.rel_path(path)?;
        let state = self.blames.entry(path.to_path_buf()).or_default();
        if let Some(result) = task::poll(&mut state.task) {
            match result {
                Ok(b) => {
                    state.blame = Some(b);
                    state.error = None;
                }
                Err(e) => state.error = Some(e),
            }
        }
        let hash = text_hash(text);
        let stale = state.text_hash != Some(hash) || state.revision != sc.revision;
        if stale && state.task.is_none() {
            if state.text_hash != Some(hash) && state.blame.is_some() {
                // Wait for typing to pause.
                let changed = *state.changed_at.get_or_insert_with(Instant::now);
                if changed.elapsed() < BLAME_DEBOUNCE {
                    sc_repaint(sc);
                    return state.blame.clone();
                }
            }
            state.changed_at = None;
            state.text_hash = Some(hash);
            state.revision = sc.revision;
            state.task = sc.load_blame(&rel, text.to_owned());
        }
        state.blame.clone()
    }

    pub fn blame_error(&self, path: &Path) -> Option<&str> {
        self.blames.get(path)?.error.as_deref()
    }

    pub fn blame_loading(&self, path: &Path) -> bool {
        self.blames.get(path).is_some_and(|s| s.task.is_some())
    }
}

fn sc_repaint(sc: &SourceControl) {
    sc.request_repaint_after(BLAME_DEBOUNCE);
}

/// Paints the blame column to the left of the editor text. `x` is the
/// column's horizontal range. Returns a commit to open when clicked.
pub fn paint_blame(
    ui: &mut Ui,
    x: egui::Rangef,
    output: &TextEditOutput,
    blame: &Blame,
    colors: &UiColors,
) -> Option<String> {
    let clip = ui.clip_rect();
    let font = FontId::proportional(11.5);
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, 'n'));
    let max_chars = ((x.span() - 16.0) / char_w) as usize;
    let now = fmt::now_unix();
    let mut clicked = None;
    let mut prev: Option<&Arc<BlameCommit>> = None;
    ui.painter().line_segment(
        [
            pos2(x.max - 1.0, clip.top()),
            pos2(x.max - 1.0, clip.bottom()),
        ],
        Stroke::new(1.0, colors.border.0),
    );
    for (line, starts, rect) in gutter::rows(output) {
        if !starts {
            continue;
        }
        let Some(commit) = blame.get(line - 1) else {
            break;
        };
        let first_of_run = prev.is_none_or(|p| !Arc::ptr_eq(p, commit));
        prev = Some(commit);
        if rect.bottom() < clip.top() {
            continue;
        }
        if rect.top() > clip.bottom() {
            break;
        }
        let row = Rect::from_x_y_ranges(x, rect.y_range());
        // Heat strip: recent commits in the accent color, old ones faded.
        let age_days = ((now - commit.time).max(0) / 86_400) as f32;
        let heat = if commit.is_uncommitted() {
            colors.modified.0
        } else {
            colors
                .accent
                .0
                .gamma_multiply((1.0 - age_days / 365.0).clamp(0.15, 1.0))
        };
        ui.painter().rect_filled(
            Rect::from_x_y_ranges(x.max - 4.0..=x.max - 2.0, rect.y_range()),
            0,
            heat,
        );
        if first_of_run {
            if line > 1 {
                ui.painter().line_segment(
                    [pos2(x.min, rect.top()), pos2(x.max - 6.0, rect.top())],
                    Stroke::new(1.0, colors.border.0.gamma_multiply(0.6)),
                );
            }
            let text = if commit.is_uncommitted() {
                "Uncommitted changes".to_owned()
            } else {
                format!("{} · {}", fmt::relative_time(commit.time), commit.author)
            };
            let text: String = if text.chars().count() > max_chars {
                text.chars()
                    .take(max_chars.saturating_sub(1))
                    .collect::<String>()
                    + "…"
            } else {
                text
            };
            ui.painter().text(
                pos2(x.min + 6.0, rect.center().y),
                Align2::LEFT_CENTER,
                text,
                font.clone(),
                if commit.is_uncommitted() {
                    colors.modified.0
                } else {
                    colors.text_muted.0
                },
            );
        }
        if commit.is_uncommitted() {
            continue;
        }
        let response = ui.interact(row, egui::Id::unique(("blame", line)), Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(row, 0, colors.widget_hover.0.gamma_multiply(0.35));
        }
        let response = response.on_hover_ui(|ui| {
            ui.set_max_width(420.0);
            ui.label(RichText::new(&commit.summary).strong());
            ui.label(format!("{} <{}>", commit.author, commit.email));
            ui.label(
                RichText::new(format!(
                    "{} · {}",
                    commit.short(),
                    fmt::relative_time(commit.time)
                ))
                .color(colors.text_muted.0),
            );
            ui.label(RichText::new("Click to open the commit").small().weak());
        });
        if response.clicked() {
            clicked = Some(commit.hash.clone());
        }
    }
    clicked
}

/// Tints conflict blocks and puts Accept buttons on each `<<<<<<<` line.
pub fn paint_conflicts(
    ui: &mut Ui,
    output: &TextEditOutput,
    conflicts: &[Conflict],
    colors: &UiColors,
) -> Option<(Conflict, Resolution)> {
    let clip = ui.clip_rect();
    let mut chosen = None;
    let rects: Vec<(usize, Rect)> = gutter::rows(output)
        .filter(|(_, _, r)| r.bottom() >= clip.top() && r.top() <= clip.bottom())
        .map(|(l, _, r)| (l - 1, r))
        .collect();
    let frame = output.response.response.rect;
    for c in conflicts {
        let markers = [Some(c.start), c.base, Some(c.mid), Some(c.end)];
        for (line, r) in &rects {
            let fill = if markers.contains(&Some(*line)) {
                colors.text_muted.0.gamma_multiply(0.22)
            } else if c.current().contains(line) {
                colors.modified.0.gamma_multiply(0.16)
            } else if c.incoming().contains(line) {
                colors.added.0.gamma_multiply(0.16)
            } else if (c.start..c.end).contains(line) {
                // diff3 base section.
                colors.text_muted.0.gamma_multiply(0.10)
            } else {
                continue;
            };
            let row = Rect::from_x_y_ranges(output.galley_pos.x..=frame.right(), r.y_range());
            ui.painter().rect_filled(row, 0, fill);
        }
        let Some((_, r)) = rects.iter().find(|(l, _)| *l == c.start) else {
            continue;
        };
        // Buttons after the marker text on the `<<<<<<<` line.
        let text_w = 12.0 + c.current_label.len() as f32 * 8.0;
        let area = Rect::from_min_max(
            pos2(r.left() + 80.0 + text_w, r.top()),
            pos2(frame.right() - 8.0, r.bottom()),
        );
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(area)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.spacing_mut().button_padding = vec2(6.0, 0.0);
                let mut button = |ui: &mut Ui, label: String, how: Resolution, tip: &str| {
                    let b = egui::Button::new(RichText::new(label).small().color(colors.link.0))
                        .frame(false);
                    if ui.add(b).on_hover_text(tip).clicked() {
                        chosen = Some((c.clone(), how));
                    }
                };
                button(
                    ui,
                    "Accept Current".into(),
                    Resolution::Current,
                    &format!("Keep {}", label_or(&c.current_label, "our side")),
                );
                ui.label(RichText::new("|").small().weak());
                button(
                    ui,
                    "Accept Incoming".into(),
                    Resolution::Incoming,
                    &format!("Keep {}", label_or(&c.incoming_label, "their side")),
                );
                ui.label(RichText::new("|").small().weak());
                button(
                    ui,
                    "Accept Both".into(),
                    Resolution::Both,
                    "Keep current, then incoming",
                );
            },
        );
    }
    chosen
}

fn label_or<'a>(label: &'a str, fallback: &'a str) -> &'a str {
    if label.is_empty() { fallback } else { label }
}

/// The popup showing one change: what it replaced, with navigation and
/// Revert / Stage buttons. `anchor` is the screen rect of the change.
#[allow(clippy::too_many_arguments)]
pub fn show_peek(
    ctx: &egui::Context,
    anchor: Rect,
    editor: Rect,
    changes: &LineChanges,
    idx: usize,
    text: &str,
    can_stage: Result<(), &str>,
    colors: &UiColors,
) -> Option<EditorAction> {
    let hunk = changes.hunks.get(idx)?;
    let width = (editor.right() - anchor.left() - 16.0).clamp(280.0, 760.0);
    let pos = pos2(
        anchor.left(),
        anchor
            .bottom()
            .min(editor.bottom() - 60.0)
            .max(editor.top()),
    );
    let mut action = None;
    let font = FontId::monospace(12.5);
    egui::Area::new(egui::Id::unique("change_peek"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .stroke(Stroke::new(1.0, mark_color(hunk.kind, colors)))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        let what = match hunk.kind {
                            Mark::Added => format!("Added {} line(s)", hunk.len),
                            Mark::Modified => {
                                format!("Changed {} line(s)", hunk.len.max(hunk.old_lines.len()))
                            }
                            Mark::DeletedAbove => {
                                format!("Deleted {} line(s)", hunk.old_lines.len())
                            }
                        };
                        ui.label(
                            RichText::new(what)
                                .strong()
                                .color(mark_color(hunk.kind, colors)),
                        );
                        ui.label(
                            RichText::new(format!("Change {} of {}", idx + 1, changes.hunks.len()))
                                .small()
                                .color(colors.text_muted.0),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if icons::button(ui, Icon::Close, "Close (Esc)").clicked() {
                                action = Some(EditorAction::ClosePeek);
                            }
                            let n = changes.hunks.len();
                            if icons::button(ui, Icon::ArrowDown, "Next change (Alt+F5)").clicked()
                            {
                                action = Some(EditorAction::GoToChange((idx + 1) % n));
                            }
                            if icons::button(ui, Icon::ArrowUp, "Previous change (Shift+Alt+F5)")
                                .clicked()
                            {
                                action = Some(EditorAction::GoToChange((idx + n - 1) % n));
                            }
                            ui.add_space(6.0);
                            let stage = ui
                                .add_enabled(can_stage.is_ok(), egui::Button::new("Stage").small())
                                .on_hover_text("Stage just this change")
                                .on_disabled_hover_text(can_stage.err().unwrap_or_default());
                            if stage.clicked() {
                                action = Some(EditorAction::Stage(hunk.clone()));
                            }
                            if ui
                                .add(egui::Button::new("Revert").small())
                                .on_hover_text("Restore the committed lines in the editor")
                                .clicked()
                            {
                                action = Some(EditorAction::Revert(hunk.clone()));
                            }
                        });
                    });
                    ui.separator();
                    let current: Vec<&str> = text
                        .split('\n')
                        .skip(hunk.start - 1)
                        .take(hunk.len)
                        .collect();
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for l in &hunk.old_lines {
                                diff_line(ui, "−", l, colors.deleted.0, &font, colors);
                            }
                            for l in &current {
                                diff_line(ui, "+", l, colors.added.0, &font, colors);
                            }
                        });
                });
        });
    action
}

fn diff_line(
    ui: &mut Ui,
    sign: &str,
    text: &str,
    color: Color32,
    font: &FontId,
    colors: &UiColors,
) {
    let h = ui.fonts_mut(|f| f.row_height(font)) + 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    ui.painter()
        .rect_filled(rect, 0, color.gamma_multiply(0.16));
    ui.painter().text(
        rect.left_center() + vec2(6.0, 0.0),
        Align2::LEFT_CENTER,
        sign,
        font.clone(),
        color,
    );
    ui.painter().text(
        rect.left_center() + vec2(22.0, 0.0),
        Align2::LEFT_CENTER,
        text.trim_end_matches('\r').replace('\t', "    "),
        font.clone(),
        colors.text.0,
    );
}

pub fn mark_color(mark: Mark, colors: &UiColors) -> Color32 {
    match mark {
        Mark::Added => colors.added.0,
        Mark::Modified => colors.modified.0,
        Mark::DeletedAbove => colors.deleted.0,
    }
}

/// Character index of the start of 1-based `line`, for placing the cursor.
pub fn line_start_char(text: &str, line: usize) -> usize {
    let mut chars = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i + 1 == line {
            return chars;
        }
        chars += l.chars().count() + 1;
    }
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_start_chars() {
        let t = "ab\nçd\n\nx";
        assert_eq!(line_start_char(t, 1), 0);
        assert_eq!(line_start_char(t, 2), 3);
        assert_eq!(line_start_char(t, 3), 6);
        assert_eq!(line_start_char(t, 4), 7);
        assert_eq!(line_start_char(t, 99), 8);
    }
}

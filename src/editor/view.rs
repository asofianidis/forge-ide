//! The state of one editor view onto a document: its cursors, folded
//! regions, and the mapping between document lines and the rows on screen
//! (which differ once lines are folded away or wrapped).
//!
//! Several views can show the same document (split editors); each follows
//! the document's change log, so edits made in one view, by undo, or by a
//! language server keep the others' cursors and folds in place.
//!
//! Everything here works in chars and "cells" (columns: a tab is 4 cells,
//! like egui draws it, other chars one), so it is independent of fonts and
//! can be tested without a UI.

use std::ops::Range;

use super::Buffer;
use super::buffer::Change;
use super::selection::{Cursor, Selections};

/// Width of a tab in cells (egui draws a tab as four spaces).
pub const TAB_CELLS: usize = 4;

pub fn cells(c: char) -> usize {
    if c == '\t' { TAB_CELLS } else { 1 }
}

/// Char offsets (within the line) where each wrapped row after the first
/// starts, wrapping `line` at `cols` cells. Breaks after whitespace where
/// possible, otherwise mid-word.
pub fn wrap_points(line: &str, cols: usize) -> Vec<usize> {
    let cols = cols.max(1);
    let mut breaks = Vec::new();
    let mut row_start = 0;
    let mut width = 0;
    let mut after_space: Option<usize> = None;
    let mut widths: Vec<usize> = Vec::new();
    for (i, c) in line.chars().enumerate() {
        let w = cells(c);
        widths.push(w);
        if width + w > cols && i > row_start {
            let at = match after_space {
                Some(b) if b > row_start && b <= i => b,
                _ => i,
            };
            breaks.push(at);
            width = widths[at..i].iter().sum();
            row_start = at;
            after_space = None;
        }
        width += w;
        if c == ' ' || c == '\t' {
            after_space = Some(i + 1);
        }
    }
    breaks
}

/// A folded region: line `start` stays visible (with a marker), lines
/// `start + 1..=end` are hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub start: usize,
    pub end: usize,
}

impl Fold {
    pub fn hides(&self, line: usize) -> bool {
        line > self.start && line <= self.end
    }

    /// Follows an edit; `None` when the edit broke the fold (touched its
    /// first line's line break or its last line).
    fn map(self, c: &Change) -> Option<Fold> {
        let edit_end = c.line + c.removed_lines;
        let delta = c.inserted_lines as isize - c.removed_lines as isize;
        let shift = |l: usize| (l as isize + delta) as usize;
        if edit_end < self.start {
            Some(Fold {
                start: shift(self.start),
                end: shift(self.end),
            })
        } else if c.line > self.end {
            Some(self)
        } else if c.line == self.start && edit_end == self.start && c.inserted_lines == 0 {
            // Typing on the fold's first line.
            Some(self)
        } else if c.line > self.start && edit_end <= self.end {
            // Inside the folded body.
            let end = shift(self.end);
            (end > self.start).then_some(Fold {
                start: self.start,
                end,
            })
        } else {
            None
        }
    }
}

/// Rows per line for word wrap at some width, kept in step with the text.
#[derive(Debug, Clone, Default)]
struct WrapCache {
    cols: usize,
    /// Rows per line; 0 marks a line to measure again.
    rows: Vec<u32>,
}

/// Where a document line sits on screen.
#[derive(Debug, Clone, Default)]
pub struct DisplayMap {
    /// `prefix[l]` is the display row where line `l` starts, with one extra
    /// entry for the total; `None` when every line is one row (no folds, no
    /// wrapping).
    prefix: Option<Vec<usize>>,
    lines: usize,
}

impl DisplayMap {
    pub fn total_rows(&self) -> usize {
        match &self.prefix {
            Some(p) => p[self.lines],
            None => self.lines,
        }
    }

    /// First display row of `line` (where a hidden line would be).
    pub fn row_of_line(&self, line: usize) -> usize {
        let line = line.min(self.lines);
        match &self.prefix {
            Some(p) => p[line],
            None => line,
        }
    }

    /// Rows `line` takes up (0 if folded away).
    #[cfg(test)]
    pub fn rows_of_line(&self, line: usize) -> usize {
        match &self.prefix {
            Some(p) if line < self.lines => p[line + 1] - p[line],
            Some(_) => 0,
            None => usize::from(line < self.lines),
        }
    }

    /// The visible line containing display `row`, and the row within it.
    /// Rows past the end give the last visible line's last row.
    pub fn line_at_row(&self, row: usize) -> (usize, usize) {
        let Some(p) = &self.prefix else {
            let line = row.min(self.lines.saturating_sub(1));
            return (line, 0);
        };
        let total = p[self.lines];
        if total == 0 {
            return (0, 0);
        }
        let row = row.min(total - 1);
        // Last line starting at or before `row` that has rows.
        let line = p[..self.lines].partition_point(|&start| start <= row) - 1;
        (line, row - p[line])
    }
}

/// Things the view asked the widget to do on the next frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reveal {
    /// Scroll the primary cursor into view, minimally.
    Cursor,
    /// Scroll the primary cursor to the middle of the screen.
    Center,
}

pub struct EditorView {
    /// Identifies the view's widget (focus, scrolling).
    pub id: egui::Id,
    /// The document shown.
    pub doc: u64,
    pub sel: Selections,
    /// Sorted by start; nested folds are allowed.
    folds: Vec<Fold>,
    /// Buffer version that cursors, folds and the wrap cache match.
    version: u64,
    wrap: Option<WrapCache>,
    /// Inputs `map` was built from: buffer version, folds, wrap columns.
    map_key: Option<(u64, Vec<Fold>, usize)>,
    map: DisplayMap,
    /// Widest line seen, so horizontal scrolling doesn't jump around.
    pub max_width: f32,
    pub reveal: Option<Reveal>,
    /// IME composition in progress, shown at the caret.
    pub preedit: Option<String>,
    /// Pointer interaction in progress.
    pub(super) drag: Option<super::widget::Drag>,
    pub(super) clicks: super::widget::Clicks,
    /// Time of the last caret movement or edit, for caret blinking.
    pub(super) last_activity: f64,
}

impl EditorView {
    pub fn new(doc: u64) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            id: egui::Id::unique(("editor_view", n)),
            doc,
            sel: Selections::default(),
            folds: Vec::new(),
            version: 0,
            wrap: None,
            map_key: None,
            map: DisplayMap::default(),
            max_width: 0.0,
            reveal: None,
            preedit: None,
            drag: None,
            clicks: Default::default(),
            last_activity: 0.0,
        }
    }

    /// Follows the buffer's edits since the last call: maps cursors and
    /// folds, and marks wrapped lines to measure again.
    pub fn sync(&mut self, buf: &Buffer) {
        if self.version == buf.version() {
            return;
        }
        match buf.changes_since(self.version) {
            Some(changes) => {
                for c in changes {
                    self.sel.map(c);
                    self.folds = self.folds.iter().filter_map(|f| f.map(c)).collect();
                    if let Some(w) = &mut self.wrap {
                        let a = c.line.min(w.rows.len());
                        let b = (c.line + c.removed_lines + 1).min(w.rows.len());
                        w.rows
                            .splice(a..b, std::iter::repeat_n(0, c.inserted_lines + 1));
                    }
                }
            }
            None => {
                // Too far behind to follow: keep what still fits.
                self.folds.clear();
                self.wrap = None;
            }
        }
        self.sel.clamp(buf.len_chars());
        let lines = buf.line_count();
        self.folds.retain(|f| f.end < lines);
        self.version = buf.version();
    }

    /// The cursors in the current text, following edits not yet synced.
    pub fn current_selection(&self, buf: &Buffer) -> Selections {
        let mut sel = self.sel.clone();
        if self.version != buf.version()
            && let Some(changes) = buf.changes_since(self.version)
        {
            for c in changes {
                sel.map(c);
            }
        }
        sel.clamp(buf.len_chars());
        sel
    }

    /// Replaces the cursors (offsets in the current text) and scrolls the
    /// primary one into view.
    pub fn set_selections(&mut self, buf: &Buffer, sel: Selections, reveal: Reveal) {
        self.sync(buf);
        self.sel = sel;
        self.sel.clamp(buf.len_chars());
        self.unfold_cursors(buf);
        self.reveal = Some(reveal);
    }

    // ---- folding -----------------------------------------------------------

    pub fn folds(&self) -> &[Fold] {
        &self.folds
    }

    pub fn is_folded_at(&self, line: usize) -> bool {
        self.folds.iter().any(|f| f.start == line)
    }

    pub fn is_hidden(&self, line: usize) -> bool {
        self.folds.iter().any(|f| f.hides(line))
    }

    /// Folds lines `start + 1..=end`. Cursors inside move to the fold's
    /// first line.
    pub fn fold(&mut self, buf: &Buffer, start: usize, end: usize) {
        self.sync(buf);
        if end <= start || end >= buf.line_count() || self.is_folded_at(start) {
            return;
        }
        let fold = Fold { start, end };
        let at = self
            .folds
            .partition_point(|f| (f.start, f.end) < (start, end));
        self.folds.insert(at, fold);
        let header_end = buf.line_start(start) + buf.line_len(start);
        let hidden = buf.line_start(start + 1)..buf.line_start(end) + buf.line_len(end) + 1;
        self.sel.update(|c| {
            for p in [&mut c.anchor, &mut c.head] {
                if hidden.contains(p) {
                    *p = header_end;
                }
            }
        });
    }

    /// Unfolds the fold starting at `line`. Returns whether there was one.
    pub fn unfold(&mut self, line: usize) -> bool {
        let before = self.folds.len();
        self.folds.retain(|f| f.start != line);
        self.folds.len() != before
    }

    pub fn unfold_all(&mut self) {
        self.folds.clear();
    }

    /// Opens any fold that hides a cursor.
    pub fn unfold_cursors(&mut self, buf: &Buffer) {
        if self.folds.is_empty() {
            return;
        }
        let lines: Vec<usize> = self
            .sel
            .iter()
            .flat_map(|c| [buf.char_to_line(c.anchor), buf.char_to_line(c.head)])
            .collect();
        self.folds.retain(|f| !lines.iter().any(|&l| f.hides(l)));
    }

    // ---- display rows ----------------------------------------------------

    /// The line ↔ row mapping for the current text, folds and wrap width
    /// (`None` for no wrapping). Call [`Self::sync`] first.
    pub fn display_map(&mut self, buf: &Buffer, wrap_cols: Option<usize>) -> &DisplayMap {
        let lines = buf.line_count();
        match wrap_cols {
            Some(cols) => {
                let w = self.wrap.get_or_insert_with(WrapCache::default);
                if w.cols != cols || w.rows.len() != lines {
                    w.cols = cols;
                    w.rows.clear();
                    w.rows.resize(lines, 0);
                }
                for (line, rows) in w.rows.iter_mut().enumerate() {
                    if *rows == 0 {
                        *rows = wrap_points(&buf.line_text(line), cols).len() as u32 + 1;
                    }
                }
            }
            None => self.wrap = None,
        }
        let key = (buf.version(), self.folds.clone(), wrap_cols.unwrap_or(0));
        if self.map_key.as_ref() != Some(&key) || self.map.lines != lines {
            self.map = if self.folds.is_empty() && self.wrap.is_none() {
                DisplayMap {
                    prefix: None,
                    lines,
                }
            } else {
                let mut hidden = vec![false; lines];
                for f in &self.folds {
                    for h in hidden.iter_mut().take(f.end + 1).skip(f.start + 1) {
                        *h = true;
                    }
                }
                let mut prefix = Vec::with_capacity(lines + 1);
                let mut row = 0;
                for (line, &h) in hidden.iter().enumerate() {
                    prefix.push(row);
                    if !h {
                        row += self.wrap.as_ref().map_or(1, |w| w.rows[line] as usize);
                    }
                }
                prefix.push(row);
                DisplayMap {
                    prefix: Some(prefix),
                    lines,
                }
            };
            self.map_key = Some(key);
        }
        &self.map
    }

    /// Wrap columns of the last [`Self::display_map`] call.
    pub fn wrap_cols(&self) -> Option<usize> {
        self.wrap.as_ref().map(|w| w.cols)
    }
}

/// The rows of one line: char ranges (within the line) for each wrapped row.
pub fn line_rows(text: &str, wrap_cols: Option<usize>) -> Vec<Range<usize>> {
    let len = text.chars().count();
    let breaks = wrap_cols.map(|c| wrap_points(text, c)).unwrap_or_default();
    let mut rows = Vec::with_capacity(breaks.len() + 1);
    let mut start = 0;
    for b in breaks {
        rows.push(start..b);
        start = b;
    }
    rows.push(start..len);
    rows
}

/// Cells from the start of `row` (char range of `text`) to char `col` in it.
pub fn cells_to(text: &str, row: Range<usize>, col: usize) -> usize {
    text.chars()
        .skip(row.start)
        .take(col.saturating_sub(row.start))
        .map(cells)
        .sum()
}

/// The char in `row` closest to `goal` cells from its start.
pub fn col_at_cells(text: &str, row: Range<usize>, goal: usize, last_row: bool) -> usize {
    let mut width = 0;
    for (i, c) in text.chars().enumerate().skip(row.start).take(row.len()) {
        let w = cells(c);
        if width >= goal {
            return i;
        }
        if width + w > goal {
            // Inside a tab: whichever edge is closer.
            return if (goal - width) * 2 < w { i } else { i + 1 };
        }
        width += w;
    }
    // Past the end: the end of the row, or just before a wrapped row's break
    // (whose position is the start of the next row).
    if last_row || row.is_empty() {
        row.end
    } else {
        row.end - 1
    }
}

/// Vertical movement: the char offset `delta` display rows from `cursor`'s
/// head, keeping its goal column. Returns the new offset and goal.
pub fn move_vertically(
    buf: &Buffer,
    map: &DisplayMap,
    wrap_cols: Option<usize>,
    head: usize,
    goal: Option<usize>,
    delta: isize,
) -> (usize, usize) {
    let line = buf.char_to_line(head);
    let text = buf.line_text(line);
    let col = head - buf.line_start(line);
    let rows = line_rows(&text, wrap_cols);
    let row_in_line = rows.iter().rposition(|r| r.start <= col).unwrap_or(0);
    let goal = goal.unwrap_or_else(|| cells_to(&text, rows[row_in_line].clone(), col));
    let row = map.row_of_line(line) + row_in_line;
    let target = row as isize + delta;
    if target < 0 {
        return (0, goal);
    }
    if target as usize >= map.total_rows() {
        return (buf.len_chars(), goal);
    }
    let (t_line, t_row) = map.line_at_row(target as usize);
    let t_text = buf.line_text(t_line);
    let t_rows = line_rows(&t_text, wrap_cols);
    let t_row = t_row.min(t_rows.len() - 1);
    let col = col_at_cells(
        &t_text,
        t_rows[t_row].clone(),
        goal,
        t_row + 1 == t_rows.len(),
    );
    (buf.line_start(t_line) + col, goal)
}

/// Word-character test used for word movement and double-click selection.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Start of the word before `at` (skipping whitespace first), like
/// Alt/Ctrl+Left. Stops at line starts.
pub fn word_left(buf: &Buffer, at: usize) -> usize {
    let line = buf.char_to_line(at);
    let start = buf.line_start(line);
    if at == start {
        return at.saturating_sub(1);
    }
    let text: Vec<char> = buf.slice(start..at).chars().collect();
    let mut i = text.len();
    while i > 0 && text[i - 1].is_whitespace() {
        i -= 1;
    }
    if i > 0 {
        let word = is_word_char(text[i - 1]);
        while i > 0 && !text[i - 1].is_whitespace() && is_word_char(text[i - 1]) == word {
            i -= 1;
        }
    }
    start + i
}

/// End of the word after `at`, like Alt/Ctrl+Right. Stops at line ends.
pub fn word_right(buf: &Buffer, at: usize) -> usize {
    let line = buf.char_to_line(at);
    let end = buf.line_start(line) + buf.line_len(line);
    if at >= end {
        return (at + 1).min(buf.len_chars());
    }
    let text: Vec<char> = buf.slice(at..end).chars().collect();
    let mut i = 0;
    while i < text.len() && text[i].is_whitespace() {
        i += 1;
    }
    if i < text.len() {
        let word = is_word_char(text[i]);
        while i < text.len() && !text[i].is_whitespace() && is_word_char(text[i]) == word {
            i += 1;
        }
    }
    at + i
}

/// The word (or run of other non-space chars, or of spaces) around `at`, for
/// double-click.
pub fn word_range(buf: &Buffer, at: usize) -> Range<usize> {
    let line = buf.char_to_line(at);
    let start = buf.line_start(line);
    let text: Vec<char> = buf.line_text(line).chars().collect();
    let col = (at - start).min(text.len());
    let probe = if col < text.len() {
        col
    } else if col > 0 {
        col - 1
    } else {
        return at..at;
    };
    let class = |c: char| {
        if is_word_char(c) {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let k = class(text[probe]);
    let mut a = probe;
    while a > 0 && class(text[a - 1]) == k {
        a -= 1;
    }
    let mut b = probe + 1;
    while b < text.len() && class(text[b]) == k {
        b += 1;
    }
    start + a..start + b
}

/// Smart Home: the first non-blank char of the line, or the line start when
/// already there.
pub fn smart_home(buf: &Buffer, at: usize) -> usize {
    let line = buf.char_to_line(at);
    let start = buf.line_start(line);
    let indent = buf
        .line_text(line)
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    if at == start + indent && indent > 0 {
        start
    } else {
        start + indent
    }
}

/// The cursor for a whole line selection (including its line break).
pub fn line_selection(buf: &Buffer, line: usize) -> Cursor {
    let start = buf.line_start(line);
    let end = if line + 1 < buf.line_count() {
        buf.line_start(line + 1)
    } else {
        buf.len_chars()
    };
    Cursor::new(start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditKind;

    #[test]
    fn wraps_at_spaces_when_possible() {
        assert_eq!(wrap_points("aaaa bbbb", 6), vec![5]);
        assert_eq!(wrap_points("aaaaaaaaaa", 4), vec![4, 8]);
        assert_eq!(wrap_points("short", 10), Vec::<usize>::new());
        assert_eq!(wrap_points("", 10), Vec::<usize>::new());
        // A tab is four cells.
        assert_eq!(wrap_points("\t\tx", 8), vec![2]);
        assert_eq!(line_rows("aaaa bbbb", Some(6)), vec![0..5, 5..9]);
    }

    #[test]
    fn display_map_with_folds_and_wrap() {
        let buf = Buffer::from("a\nb\nc\nd\ne");
        let mut view = EditorView::new(1);
        view.sync(&buf);
        assert_eq!(view.display_map(&buf, None).total_rows(), 5);
        assert!(view.display_map(&buf, None).prefix.is_none());
        view.fold(&buf, 1, 3);
        let map = view.display_map(&buf, None);
        assert_eq!(map.total_rows(), 2 + 1);
        assert_eq!(map.row_of_line(4), 2);
        assert_eq!(map.line_at_row(1), (1, 0));
        assert_eq!(map.line_at_row(2), (4, 0));
        assert_eq!(map.rows_of_line(2), 0);

        let long = Buffer::from("aaaa bbbb\nc");
        let mut view = EditorView::new(1);
        view.sync(&long);
        let map = view.display_map(&long, Some(6));
        assert_eq!(map.total_rows(), 3);
        assert_eq!(map.line_at_row(1), (0, 1));
        assert_eq!(map.line_at_row(2), (1, 0));
    }

    #[test]
    fn wrap_cache_follows_edits() {
        let mut buf = Buffer::from("aaaa bbbb\nc");
        let mut view = EditorView::new(1);
        view.sync(&buf);
        assert_eq!(view.display_map(&buf, Some(6)).total_rows(), 3);
        buf.apply_edits([(11..11, " dddd eeee\nf")]);
        view.sync(&buf);
        // "c dddd eeee" wraps to three rows.
        assert_eq!(view.display_map(&buf, Some(6)).total_rows(), 6);
    }

    #[test]
    fn folds_follow_edits() {
        let mut buf = Buffer::from("0\n1\n2\n3\n4\n5");
        let mut view = EditorView::new(1);
        view.sync(&buf);
        view.fold(&buf, 2, 4);
        // A line added above shifts the fold.
        buf.apply_edits([(0..0, "new\n")]);
        view.sync(&buf);
        assert_eq!(view.folds(), &[Fold { start: 3, end: 5 }]);
        // Typing on the first line keeps it.
        let at = buf.line_start(3);
        buf.edit([(at..at, "x")], EditKind::Typing, None);
        view.sync(&buf);
        assert_eq!(view.folds(), &[Fold { start: 3, end: 5 }]);
        // A line added inside grows it.
        let at = buf.line_start(4);
        buf.apply_edits([(at..at, "in\n")]);
        view.sync(&buf);
        assert_eq!(view.folds(), &[Fold { start: 3, end: 6 }]);
        // Deleting its last line's break unfolds.
        let end = buf.line_start(7);
        buf.apply_edits([(end - 1..end, "")]);
        view.sync(&buf);
        assert!(view.folds().is_empty());
    }

    #[test]
    fn folding_moves_hidden_cursors_and_moving_into_a_fold_opens_it() {
        let buf = Buffer::from("f {\n  a\n  b\n}\n");
        let mut view = EditorView::new(1);
        view.set_selections(&buf, Selections::single(Cursor::caret(8)), Reveal::Cursor);
        view.fold(&buf, 0, 2);
        assert_eq!(view.sel.primary().head, 3, "end of the fold's first line");
        view.set_selections(&buf, Selections::single(Cursor::caret(6)), Reveal::Cursor);
        assert!(view.folds().is_empty());
    }

    #[test]
    fn vertical_movement_keeps_goal_column_and_skips_folds() {
        let buf = Buffer::from("abcdef\nab\nabcdef\nx\ny");
        let mut view = EditorView::new(1);
        view.sync(&buf);
        let map = view.display_map(&buf, None).clone();
        let (down, goal) = move_vertically(&buf, &map, None, 5, None, 1);
        assert_eq!((down, goal), (9, 5), "clamped to the short line");
        let (down2, _) = move_vertically(&buf, &map, None, down, Some(goal), 1);
        assert_eq!(down2, 15, "goal column restored");
        let (top, _) = move_vertically(&buf, &map, None, 2, None, -1);
        assert_eq!(top, 0);
        let (bottom, _) = move_vertically(&buf, &map, None, 18, None, 5);
        assert_eq!(bottom, buf.len_chars());

        view.fold(&buf, 1, 3);
        let map = view.display_map(&buf, None).clone();
        let (down, _) = move_vertically(&buf, &map, None, 9, None, 1);
        assert_eq!(buf.char_to_line(down), 4, "skips the folded lines");
    }

    #[test]
    fn vertical_movement_through_wrapped_rows() {
        let buf = Buffer::from("aaaa bbbb\nc");
        let mut view = EditorView::new(1);
        view.sync(&buf);
        let map = view.display_map(&buf, Some(6)).clone();
        let (down, _) = move_vertically(&buf, &map, Some(6), 1, None, 1);
        assert_eq!(down, 6, "same column on the wrapped row");
        let (down, _) = move_vertically(&buf, &map, Some(6), down, Some(1), 1);
        assert_eq!(down, 11);
    }

    #[test]
    fn word_movement_and_selection() {
        let buf = Buffer::from("let foo_bar = x.y;\n  next");
        assert_eq!(word_right(&buf, 0), 3);
        assert_eq!(word_right(&buf, 3), 11);
        assert_eq!(word_right(&buf, 11), 13);
        assert_eq!(word_left(&buf, 11), 4);
        assert_eq!(word_left(&buf, 4), 0);
        assert_eq!(word_right(&buf, 18), 19, "line end steps to the next line");
        assert_eq!(word_left(&buf, 19), 18);
        assert_eq!(word_range(&buf, 6), 4..11);
        assert_eq!(word_range(&buf, 15), 15..16);
        assert_eq!(smart_home(&buf, 25), 21);
        assert_eq!(smart_home(&buf, 21), 19);
        assert_eq!(line_selection(&buf, 0), Cursor::new(0, 19));
    }
}

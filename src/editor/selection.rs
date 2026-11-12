//! Cursors and selections of an editor view, as char offsets.
//!
//! A view always has at least one cursor. With several (multi-cursor
//! editing), they are kept sorted by position and never overlap: cursors that
//! run into each other merge.

use std::ops::Range;

use super::buffer::{Change, SelectionSnapshot};
use super::ops::Selection;

/// One cursor: the selection runs from `anchor` (where it started) to `head`
/// (where the caret is). Equal means just a caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub anchor: usize,
    pub head: usize,
    /// Column (in cells) vertical movement aims for, so moving through a
    /// short line doesn't lose the column.
    pub goal: Option<usize>,
}

impl Cursor {
    pub fn caret(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
            goal: None,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal: None,
        }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn selection(&self) -> Selection {
        Selection::new(self.anchor, self.head)
    }

    /// Moves the caret to `to`; with `extend` the anchor stays, otherwise
    /// the selection collapses.
    pub fn move_to(&mut self, to: usize, extend: bool) {
        self.head = to;
        if !extend {
            self.anchor = to;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selections {
    /// Sorted by position, non-overlapping, never empty.
    cursors: Vec<Cursor>,
    /// Index of the primary cursor: the one the status bar, scrolling and
    /// single-cursor commands follow.
    primary: usize,
}

impl Default for Selections {
    fn default() -> Self {
        Self::single(Cursor::caret(0))
    }
}

impl Selections {
    pub fn single(cursor: Cursor) -> Self {
        Self {
            cursors: vec![cursor],
            primary: 0,
        }
    }

    /// Cursors from `list`, with `primary` indexing into it. Overlapping
    /// cursors merge.
    pub fn new(list: Vec<Cursor>, primary: usize) -> Self {
        if list.is_empty() {
            return Self::default();
        }
        let primary = primary.min(list.len() - 1);
        let mut sel = Self {
            cursors: list,
            primary,
        };
        sel.normalize();
        sel
    }

    pub fn from_snapshot(snapshot: &[(usize, usize)]) -> Self {
        let list: Vec<Cursor> = snapshot.iter().map(|&(a, h)| Cursor::new(a, h)).collect();
        let primary = list.len().saturating_sub(1);
        Self::new(list, primary)
    }

    pub fn snapshot(&self) -> SelectionSnapshot {
        self.cursors.iter().map(|c| (c.anchor, c.head)).collect()
    }

    pub fn primary(&self) -> Cursor {
        self.cursors[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn len(&self) -> usize {
        self.cursors.len()
    }

    pub fn is_multi(&self) -> bool {
        self.cursors.len() > 1
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cursor> {
        self.cursors.iter()
    }

    pub fn as_slice(&self) -> &[Cursor] {
        &self.cursors
    }

    /// Changes every cursor with `f`, then merges any that now overlap.
    pub fn update(&mut self, mut f: impl FnMut(&mut Cursor)) {
        for c in &mut self.cursors {
            f(c);
        }
        self.normalize();
    }

    /// Adds a cursor and makes it the primary one.
    pub fn add(&mut self, cursor: Cursor) {
        self.cursors.push(cursor);
        self.primary = self.cursors.len() - 1;
        self.normalize();
    }

    /// Replaces the primary cursor (e.g. while dragging it out).
    pub fn set_primary(&mut self, cursor: Cursor) {
        self.cursors[self.primary] = cursor;
        self.normalize();
    }

    /// Removes the cursor at `idx`, unless it is the only one.
    pub fn remove(&mut self, idx: usize) {
        if self.cursors.len() <= 1 || idx >= self.cursors.len() {
            return;
        }
        self.cursors.remove(idx);
        if self.primary >= idx && self.primary > 0 {
            self.primary -= 1;
        }
    }

    /// Keeps only the primary cursor.
    pub fn collapse_to_primary(&mut self) {
        let p = self.primary();
        *self = Self::single(p);
    }

    /// Clamps every cursor to `len` chars.
    pub fn clamp(&mut self, len: usize) {
        self.update(|c| {
            c.anchor = c.anchor.min(len);
            c.head = c.head.min(len);
        });
    }

    /// Follows a change made to the text (by another view, undo, a language
    /// server, …).
    pub fn map(&mut self, change: &Change) {
        for c in &mut self.cursors {
            if c.is_empty() {
                let at = change.map(c.head);
                *c = Cursor {
                    anchor: at,
                    head: at,
                    goal: c.goal,
                };
            } else {
                c.anchor = change.map(c.anchor);
                c.head = change.map(c.head);
            }
        }
        self.normalize();
    }

    /// Sorts the cursors and merges overlapping ones, keeping track of the
    /// primary one.
    fn normalize(&mut self) {
        if self.cursors.len() == 1 {
            return;
        }
        let primary = self.cursors[self.primary];
        let mut order: Vec<(Cursor, bool)> = self
            .cursors
            .iter()
            .enumerate()
            .map(|(i, &c)| (c, i == self.primary))
            .collect();
        order.sort_by_key(|(c, _)| (c.start(), c.end()));
        let mut merged: Vec<(Cursor, bool)> = Vec::with_capacity(order.len());
        for (c, is_primary) in order {
            if let Some((last, last_primary)) = merged.last_mut() {
                let touches = c.start() < last.end()
                    || c.start() == last.start()
                    || (c.start() == last.end() && (c.is_empty() || last.is_empty()));
                if touches {
                    let (start, end) = (last.start(), last.end().max(c.end()));
                    let keep = if is_primary { c } else { *last };
                    *last = if keep.head >= keep.anchor {
                        Cursor::new(start, end)
                    } else {
                        Cursor::new(end, start)
                    };
                    *last_primary |= is_primary;
                    continue;
                }
            }
            merged.push((c, is_primary));
        }
        self.primary = merged.iter().position(|(_, p)| *p).unwrap_or(0);
        if merged.len() == 1 {
            merged[0].0.goal = primary.goal;
        }
        self.cursors = merged.into_iter().map(|(c, _)| c).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(s: &Selections) -> Vec<(usize, usize)> {
        s.snapshot()
    }

    #[test]
    fn cursors_sort_and_merge() {
        let s = Selections::new(
            vec![
                Cursor::caret(9),
                Cursor::new(2, 5),
                Cursor::caret(4),
                Cursor::caret(9),
                Cursor::new(12, 10),
            ],
            4,
        );
        assert_eq!(ranges(&s), vec![(2, 5), (9, 9), (12, 10)]);
        assert_eq!(s.primary(), Cursor::new(12, 10));
    }

    #[test]
    fn touching_selections_stay_separate_but_carets_merge() {
        let s = Selections::new(vec![Cursor::new(0, 2), Cursor::new(2, 4)], 0);
        assert_eq!(s.len(), 2);
        let s = Selections::new(vec![Cursor::new(0, 2), Cursor::caret(2)], 1);
        assert_eq!(ranges(&s), vec![(0, 2)]);
    }

    #[test]
    fn primary_survives_a_merge() {
        let mut s = Selections::new(vec![Cursor::caret(1), Cursor::caret(5)], 1);
        s.update(|c| c.move_to(5, false));
        assert_eq!(s.len(), 1);
        assert_eq!(s.primary_index(), 0);
    }

    #[test]
    fn map_follows_edits() {
        let mut s = Selections::new(vec![Cursor::caret(1), Cursor::new(4, 6)], 0);
        s.map(&Change {
            version: 0,
            at: 0,
            removed: 0,
            inserted: 3,
            line: 0,
            removed_lines: 0,
            inserted_lines: 0,
        });
        assert_eq!(ranges(&s), vec![(4, 4), (7, 9)]);
    }

    #[test]
    fn remove_and_collapse() {
        let mut s = Selections::new(
            vec![Cursor::caret(1), Cursor::caret(3), Cursor::caret(5)],
            2,
        );
        s.remove(0);
        assert_eq!(s.primary(), Cursor::caret(5));
        s.collapse_to_primary();
        assert_eq!(ranges(&s), vec![(5, 5)]);
        s.remove(0);
        assert_eq!(s.len(), 1, "the last cursor stays");
    }
}

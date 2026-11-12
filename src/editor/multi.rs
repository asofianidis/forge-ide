//! Edits at every cursor at once.
//!
//! The operations in [`super::ops`] work on a whole text and one selection.
//! Here each runs on a small window of lines around one cursor, the result is
//! reduced to the part that changed, and the changes for all cursors are
//! combined into one transaction. This keeps typing aids and line commands
//! O(lines touched) instead of O(file size), and makes them work with any
//! number of cursors.

use std::ops::Range;

use super::Buffer;
use super::ops::{Edit, Selection};
use super::selection::{Cursor, Selections};

/// What an operation does at one cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorEdit {
    /// Nothing: the cursor stays (following other cursors' edits).
    Keep,
    /// Only the selection changes.
    Move(Cursor),
    /// Replace `range` (chars in the current text) with `text`, leaving
    /// `cursor` — given as if this were the only change.
    Replace {
        range: Range<usize>,
        text: String,
        cursor: Cursor,
    },
}

impl CursorEdit {
    /// Replaces `range` with `text`, leaving a caret after it.
    pub fn insert(range: Range<usize>, text: &str) -> Self {
        let at = range.start + text.chars().count();
        Self::Replace {
            range,
            text: text.to_owned(),
            cursor: Cursor::caret(at),
        }
    }
}

/// The combined result: edits for [`Buffer::edit`] and the cursors after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub edits: Vec<(Range<usize>, String)>,
    pub after: Selections,
}

impl Transaction {
    pub fn changes_text(&self) -> bool {
        !self.edits.is_empty()
    }
}

/// Runs `f` for every cursor and combines the results. Edits that overlap
/// an earlier cursor's edit are dropped (that cursor just follows along).
pub fn for_each_cursor(
    buf: &Buffer,
    sel: &Selections,
    mut f: impl FnMut(&Buffer, &Cursor) -> CursorEdit,
) -> Transaction {
    let results: Vec<(Cursor, CursorEdit)> = sel.iter().map(|c| (*c, f(buf, c))).collect();
    combine(results, sel.primary_index())
}

fn combine(results: Vec<(Cursor, CursorEdit)>, primary: usize) -> Transaction {
    // Accept edits in order, skipping any that overlap one already taken
    // (including a second insert at the same spot).
    let mut order: Vec<usize> = (0..results.len())
        .filter(|&i| matches!(results[i].1, CursorEdit::Replace { .. }))
        .collect();
    order.sort_by_key(|&i| match &results[i].1 {
        CursorEdit::Replace { range, .. } => (range.start, range.end),
        _ => unreachable!(),
    });
    // (range, text, growth) in text order, and each result's index in it.
    let mut accepted: Vec<(Range<usize>, String, isize)> = Vec::new();
    let mut slot: Vec<Option<usize>> = vec![None; results.len()];
    for i in order {
        let CursorEdit::Replace { range, text, .. } = &results[i].1 else {
            continue;
        };
        if let Some((prev, _, _)) = accepted.last()
            && (range.start < prev.end || (prev.is_empty() && range.start == prev.start))
        {
            continue;
        }
        slot[i] = Some(accepted.len());
        let growth = text.chars().count() as isize - range.len() as isize;
        accepted.push((range.clone(), text.clone(), growth));
    }

    // Where `pos` ends up after all accepted edits.
    let map = |pos: usize| -> usize {
        let mut shift = 0isize;
        for (r, t, growth) in &accepted {
            if pos < r.start || (pos == r.start && !r.is_empty()) {
                break;
            }
            if pos >= r.end {
                shift += growth;
            } else {
                return (r.start as isize + shift) as usize + t.chars().count();
            }
        }
        (pos as isize + shift) as usize
    };

    let mut cursors = Vec::with_capacity(results.len());
    for (i, (original, edit)) in results.iter().enumerate() {
        let cursor = match (edit, slot[i]) {
            (CursorEdit::Replace { cursor, .. }, Some(k)) => {
                // `cursor` assumes only this edit; add the growth of those before it.
                let before: isize = accepted[..k].iter().map(|(_, _, g)| g).sum();
                let at = |p: usize| (p as isize + before).max(0) as usize;
                Cursor::new(at(cursor.anchor), at(cursor.head))
            }
            (CursorEdit::Move(c), _) => Cursor {
                anchor: map(c.anchor),
                head: map(c.head),
                goal: c.goal,
            },
            _ => Cursor {
                anchor: map(original.anchor),
                head: map(original.head),
                goal: original.goal,
            },
        };
        cursors.push(cursor);
    }
    Transaction {
        edits: accepted.into_iter().map(|(r, t, _)| (r, t)).collect(),
        after: Selections::new(cursors, primary),
    }
}

/// Runs a whole-text operation from [`super::ops`] on the lines around
/// `cursor` (`before` lines above and `after` below its own) and returns the
/// part that changed. `None` from `op` means the operation doesn't apply.
pub fn local(
    buf: &Buffer,
    cursor: &Cursor,
    before: usize,
    after: usize,
    op: impl FnOnce(&str, Selection) -> Option<Edit>,
) -> Option<CursorEdit> {
    let first = buf.char_to_line(cursor.start()).saturating_sub(before);
    let last = buf.char_to_line(cursor.end()) + after;
    let w0 = buf.line_start(first);
    let w1 = if last + 1 < buf.line_count() {
        buf.line_start(last + 1)
    } else {
        buf.len_chars()
    };
    let window = buf.slice(w0..w1);
    let sel = Selection::new(cursor.anchor - w0, cursor.head - w0);
    let edit = op(&window, sel)?;
    let new = Cursor::new(w0 + edit.selection.start, w0 + edit.selection.end);
    let new = if cursor.head < cursor.anchor {
        Cursor::new(new.head, new.anchor)
    } else {
        new
    };
    // Minimal change: strip the common prefix and suffix.
    let old: Vec<char> = window.chars().collect();
    let next: Vec<char> = edit.text.chars().collect();
    let prefix = old.iter().zip(&next).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(next.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(next.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    if prefix == old.len() && prefix == next.len() {
        return Some(CursorEdit::Move(new));
    }
    Some(CursorEdit::Replace {
        range: w0 + prefix..w0 + old.len() - suffix,
        text: next[prefix..next.len() - suffix].iter().collect(),
        cursor: new,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::ops;

    fn apply(buf: &mut Buffer, tx: &Transaction) {
        buf.apply_edits(tx.edits.iter().map(|(r, t)| (r.clone(), t.as_str())));
    }

    fn carets(at: &[usize]) -> Selections {
        Selections::new(at.iter().map(|&a| Cursor::caret(a)).collect(), 0)
    }

    #[test]
    fn insert_at_every_cursor() {
        let mut buf = Buffer::from("ab\ncd\nef");
        let sel = carets(&[1, 4, 7]);
        let tx = for_each_cursor(&buf, &sel, |_, c| CursorEdit::insert(c.range(), "XY"));
        apply(&mut buf, &tx);
        assert_eq!(buf.as_str(), "aXYb\ncXYd\neXYf");
        assert_eq!(tx.after.snapshot(), vec![(3, 3), (8, 8), (13, 13)]);
    }

    #[test]
    fn replacing_selections_of_different_lengths() {
        let mut buf = Buffer::from("one two three");
        let sel = Selections::new(
            vec![Cursor::new(0, 3), Cursor::new(4, 7), Cursor::new(8, 13)],
            2,
        );
        let tx = for_each_cursor(&buf, &sel, |_, c| CursorEdit::insert(c.range(), "x"));
        apply(&mut buf, &tx);
        assert_eq!(buf.as_str(), "x x x");
        assert_eq!(tx.after.snapshot(), vec![(1, 1), (3, 3), (5, 5)]);
        assert_eq!(tx.after.primary_index(), 2);
    }

    #[test]
    fn typing_aids_run_locally_per_cursor() {
        let text = "fn a() {}\nfn b() {}\n";
        let mut buf = Buffer::from(text);
        // Carets inside each pair of braces.
        let sel = carets(&[8, 18]);
        let tx = for_each_cursor(&buf, &sel, |b, c| {
            local(b, c, 0, 0, |t, s| Some(ops::newline(t, s, "    ", "rs"))).unwrap()
        });
        apply(&mut buf, &tx);
        assert_eq!(buf.as_str(), "fn a() {\n    \n}\nfn b() {\n    \n}\n");
        let lines: Vec<(usize, usize)> = tx.after.iter().map(|c| buf.line_col(c.head)).collect();
        assert_eq!(lines, vec![(2, 5), (5, 5)]);

        // Auto-closing at several cursors, and stepping over the closers.
        let mut buf = Buffer::from("a\nb");
        let sel = carets(&[1, 3]);
        let tx = for_each_cursor(&buf, &sel, |b, c| {
            local(b, c, 0, 0, |t, s| ops::type_char(t, s, '(', &[]))
                .unwrap_or_else(|| CursorEdit::insert(c.range(), "("))
        });
        apply(&mut buf, &tx);
        assert_eq!(buf.as_str(), "a()\nb()");
        let tx = for_each_cursor(&buf, &tx.after, |b, c| {
            local(b, c, 0, 0, |t, s| ops::type_char(t, s, ')', &[]))
                .unwrap_or_else(|| CursorEdit::insert(c.range(), ")"))
        });
        assert!(!tx.changes_text());
        assert_eq!(tx.after.snapshot(), vec![(3, 3), (7, 7)]);
    }

    #[test]
    fn overlapping_line_commands_apply_once() {
        let mut buf = Buffer::from("a\nb\n");
        // Two cursors on the same line: the line is commented once.
        let sel = carets(&[0, 1]);
        let style = ops::comment_style("rs").unwrap();
        let tx = for_each_cursor(&buf, &sel, |b, c| {
            local(b, c, 0, 0, |t, s| Some(ops::toggle_comment(t, s, style))).unwrap()
        });
        apply(&mut buf, &tx);
        assert_eq!(buf.as_str(), "// a\nb\n");
        assert_eq!(tx.after.len(), 2);
    }

    #[test]
    fn move_lines_needs_context() {
        let mut buf = Buffer::from("1\n2\n3");
        let sel = carets(&[2]);
        let tx = for_each_cursor(&buf, &sel, |b, c| {
            local(b, c, 1, 1, |t, s| ops::move_lines(t, s, false)).unwrap_or(CursorEdit::Keep)
        });
        apply(&mut buf, &tx);
        assert_eq!(buf.as_str(), "1\n3\n2");
        assert_eq!(tx.after.primary().head, 4);
        // At the last line there is nothing to move past.
        let tx = for_each_cursor(&buf, &tx.after, |b, c| {
            local(b, c, 1, 1, |t, s| ops::move_lines(t, s, false)).unwrap_or(CursorEdit::Keep)
        });
        assert!(!tx.changes_text());
    }

    #[test]
    fn cursors_without_edits_follow_the_others() {
        let mut buf = Buffer::from("abc");
        let sel = carets(&[0, 3]);
        let tx = for_each_cursor(&buf, &sel, |_, c| {
            if c.head == 0 {
                CursorEdit::insert(0..0, "12")
            } else {
                CursorEdit::Keep
            }
        });
        apply(&mut buf, &tx);
        assert_eq!(tx.after.snapshot(), vec![(2, 2), (5, 5)]);
    }
}

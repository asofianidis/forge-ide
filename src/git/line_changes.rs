//! Which lines of an editor buffer differ from the committed version, for
//! the colored markers in the gutter and the change peek.

use std::time::Duration;

use similar::{DiffOp, TextDiffConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were removed just above this line.
    DeletedAbove,
}

/// One contiguous change between the committed file and the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeHunk {
    pub kind: Mark,
    /// 1-based first buffer line of the change. For a pure deletion, the
    /// line the removed text sat above.
    pub start: usize,
    /// Number of buffer lines in the change (0 for a pure deletion).
    pub len: usize,
    /// The committed lines this change replaced (empty for an addition).
    pub old_lines: Vec<String>,
}

impl ChangeHunk {
    /// Whether 1-based `line` belongs to this change.
    pub fn contains(&self, line: usize) -> bool {
        if self.len == 0 {
            line == self.start
        } else {
            (self.start..self.start + self.len).contains(&line)
        }
    }
}

/// Per-line markers for the current buffer (index 0 = line 1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineChanges {
    marks: Vec<Option<Mark>>,
    pub hunks: Vec<ChangeHunk>,
}

impl LineChanges {
    /// Marker for 1-based `line`.
    pub fn get(&self, line: usize) -> Option<Mark> {
        self.marks.get(line.checked_sub(1)?).copied().flatten()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.marks.iter().all(Option::is_none)
    }

    /// Index of the change covering 1-based `line`.
    pub fn hunk_at(&self, line: usize) -> Option<usize> {
        self.hunks.iter().position(|h| h.contains(line))
    }

    /// The first change starting after `line`, wrapping to the first.
    pub fn next_after(&self, line: usize) -> Option<usize> {
        self.hunks
            .iter()
            .position(|h| h.start > line)
            .or((!self.hunks.is_empty()).then_some(0))
    }

    /// The last change starting before `line`, wrapping to the last.
    pub fn prev_before(&self, line: usize) -> Option<usize> {
        self.hunks
            .iter()
            .rposition(|h| h.start < line)
            .or(self.hunks.len().checked_sub(1))
    }
}

/// Compares `current` against `base` line by line.
pub fn compute(base: &str, current: &str) -> LineChanges {
    let line_count = current.split('\n').count();
    let old_lines: Vec<&str> = base.split('\n').collect();
    let old = |index: usize, len: usize| -> Vec<String> {
        old_lines
            .iter()
            .skip(index)
            .take(len)
            .map(|s| s.trim_end_matches('\r').to_owned())
            .collect()
    };
    let mut marks = vec![None; line_count];
    let mut hunks = Vec::new();
    let diff = TextDiffConfig::default()
        .timeout(Duration::from_millis(50))
        .diff_lines(base, current);
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                set(&mut marks, new_index, new_len, Mark::Added);
                hunks.push(ChangeHunk {
                    kind: Mark::Added,
                    start: new_index + 1,
                    len: new_len,
                    old_lines: Vec::new(),
                });
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                set(&mut marks, new_index, new_len, Mark::Modified);
                hunks.push(ChangeHunk {
                    kind: Mark::Modified,
                    start: new_index + 1,
                    len: new_len,
                    old_lines: old(old_index, old_len),
                });
            }
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => {
                let idx = new_index.min(line_count.saturating_sub(1));
                if let Some(slot) = marks.get_mut(idx)
                    && slot.is_none()
                {
                    *slot = Some(Mark::DeletedAbove);
                }
                hunks.push(ChangeHunk {
                    kind: Mark::DeletedAbove,
                    start: idx + 1,
                    len: 0,
                    old_lines: old(old_index, old_len),
                });
            }
        }
    }
    LineChanges { marks, hunks }
}

fn set(marks: &mut [Option<Mark>], start: usize, len: usize, mark: Mark) {
    for slot in marks.iter_mut().skip(start).take(len) {
        *slot = Some(mark);
    }
}

/// `text` with change `hunk` undone: its buffer lines replaced by the
/// committed ones.
pub fn revert(text: &str, hunk: &ChangeHunk) -> String {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let start = (hunk.start - 1).min(lines.len());
    let end = (start + hunk.len).min(lines.len());
    // A pure deletion's text goes back above `start`.
    lines.splice(start..end, hunk.old_lines.iter().map(String::as_str));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_added_modified_and_deleted_lines() {
        let base = "a\nb\nc\nd\n";
        let current = "a\nB\nc\nnew\n";
        let changes = compute(base, current);
        assert_eq!(changes.get(1), None);
        assert_eq!(changes.get(2), Some(Mark::Modified));
        assert_eq!(changes.get(3), None);
        assert_eq!(changes.get(4), Some(Mark::Modified));

        let changes = compute("a\nb\nc\n", "a\nc\n");
        assert_eq!(changes.get(2), Some(Mark::DeletedAbove));

        let changes = compute("a\n", "a\nb\n");
        assert_eq!(changes.get(2), Some(Mark::Added));

        assert!(compute("same\n", "same\n").is_empty());
    }

    #[test]
    fn hunks_navigate_and_revert() {
        let base = "1\n2\n3\n4\n5\n6\n";
        let current = "1\nTWO\n3\n5\n6\nadded\n";
        let changes = compute(base, current);
        let kinds: Vec<_> = changes
            .hunks
            .iter()
            .map(|h| (h.kind, h.start, h.len))
            .collect();
        assert_eq!(
            kinds,
            [
                (Mark::Modified, 2, 1),
                (Mark::DeletedAbove, 4, 0),
                (Mark::Added, 6, 1)
            ]
        );
        assert_eq!(changes.hunks[0].old_lines, ["2"]);
        assert_eq!(changes.hunk_at(2), Some(0));
        assert_eq!(changes.hunk_at(4), Some(1));
        assert_eq!(changes.hunk_at(1), None);
        assert_eq!(changes.next_after(2), Some(1));
        assert_eq!(changes.next_after(6), Some(0), "wraps around");
        assert_eq!(changes.prev_before(2), Some(2), "wraps around");

        // Reverting every change in turn restores the committed text.
        let mut text = current.to_owned();
        while let Some(h) = compute(base, &text).hunks.first().cloned() {
            text = revert(&text, &h);
        }
        assert_eq!(text, base);
    }
}

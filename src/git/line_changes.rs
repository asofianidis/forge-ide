//! Which lines of an editor buffer differ from the committed version, for
//! the colored markers in the gutter.

use std::time::Duration;

use similar::{DiffOp, TextDiffConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were removed just above this line.
    DeletedAbove,
}

/// Per-line markers for the current buffer (index 0 = line 1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineChanges {
    marks: Vec<Option<Mark>>,
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
}

/// Compares `current` against `base` line by line.
pub fn compute(base: &str, current: &str) -> LineChanges {
    let line_count = current.split('\n').count();
    let mut marks = vec![None; line_count];
    let diff = TextDiffConfig::default()
        .timeout(Duration::from_millis(50))
        .diff_lines(base, current);
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert {
                new_index, new_len, ..
            } => set(&mut marks, new_index, new_len, Mark::Added),
            DiffOp::Replace {
                new_index, new_len, ..
            } => set(&mut marks, new_index, new_len, Mark::Modified),
            DiffOp::Delete { new_index, .. } => {
                let idx = new_index.min(line_count.saturating_sub(1));
                if let Some(slot) = marks.get_mut(idx)
                    && slot.is_none()
                {
                    *slot = Some(Mark::DeletedAbove);
                }
            }
        }
    }
    LineChanges { marks }
}

fn set(marks: &mut [Option<Mark>], start: usize, len: usize, mark: Mark) {
    for slot in marks.iter_mut().skip(start).take(len) {
        *slot = Some(mark);
    }
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
}

//! Lays out a commit graph: which lane (column) each commit sits in and the
//! line segments connecting it to its parents, row by row.

/// Drawing data for one row of the graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphRow {
    /// Lane of this commit's dot.
    pub col: usize,
    /// Segments from the top of the row (lane `.0`) to its middle (lane `.1`).
    pub top: Vec<(usize, usize)>,
    /// Segments from the middle of the row (lane `.0`) to its bottom (lane `.1`).
    pub bottom: Vec<(usize, usize)>,
    /// A merge commit (more than one parent).
    pub merge: bool,
}

impl GraphRow {
    /// Number of lanes this row touches, for sizing the graph column.
    pub fn width(&self) -> usize {
        self.top
            .iter()
            .chain(&self.bottom)
            .flat_map(|&(a, b)| [a, b])
            .chain([self.col])
            .max()
            .unwrap_or(0)
            + 1
    }
}

/// Computes rows for `commits`, given as `(hash, parent hashes)` in
/// topological order (children before parents).
pub fn layout<'a>(commits: impl IntoIterator<Item = (&'a str, &'a [String])>) -> Vec<GraphRow> {
    // Each lane holds the hash of the commit it is waiting for.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::new();
    for (hash, parents) in commits {
        let before = lanes.clone();
        let col = match lanes.iter().position(|l| l.as_deref() == Some(hash)) {
            Some(c) => c,
            None => match lanes.iter().position(Option::is_none) {
                Some(c) => c,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            },
        };
        let mut row = GraphRow {
            col,
            merge: parents.len() > 1,
            ..GraphRow::default()
        };
        // Lanes arriving from above: ones waiting for this commit join its
        // dot; the rest pass straight through.
        for (i, l) in before.iter().enumerate() {
            match l {
                Some(h) if h == hash => {
                    row.top.push((i, col));
                    if i != col {
                        lanes[i] = None;
                    }
                }
                Some(_) => row.top.push((i, i)),
                None => {}
            }
        }
        // First parent continues in this lane; others take an existing lane
        // that already waits for them, or a free one.
        lanes[col] = parents.first().cloned();
        if !parents.is_empty() {
            row.bottom.push((col, col));
        }
        for p in parents.iter().skip(1) {
            let target = match lanes.iter().position(|l| l.as_deref() == Some(p.as_str())) {
                Some(j) => j,
                None => match lanes.iter().position(Option::is_none) {
                    Some(j) => {
                        lanes[j] = Some(p.clone());
                        j
                    }
                    None => {
                        lanes.push(Some(p.clone()));
                        lanes.len() - 1
                    }
                },
            };
            row.bottom.push((col, target));
        }
        // Lanes passing through continue below.
        for (i, l) in lanes.iter().enumerate() {
            if i != col && l.is_some() && !row.bottom.iter().any(|&(_, t)| t == i) {
                row.bottom.push((i, i));
            }
        }
        // Trim trailing empty lanes so the graph doesn't grow forever.
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(commits: &[(&str, &[&str])]) -> Vec<GraphRow> {
        let owned: Vec<(String, Vec<String>)> = commits
            .iter()
            .map(|(h, ps)| (h.to_string(), ps.iter().map(|p| p.to_string()).collect()))
            .collect();
        layout(owned.iter().map(|(h, ps)| (h.as_str(), ps.as_slice())))
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let rows = run(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert!(rows.iter().all(|r| r.col == 0 && r.width() == 1));
        assert_eq!(rows[0].top, vec![]);
        assert_eq!(rows[1].top, vec![(0, 0)]);
        assert_eq!(rows[2].bottom, vec![]);
    }

    #[test]
    fn branch_and_merge_use_two_lanes() {
        // m merges f into main; f and b both branch from a.
        let rows = run(&[("m", &["b", "f"]), ("f", &["a"]), ("b", &["a"]), ("a", &[])]);
        assert!(rows[0].merge);
        assert_eq!(rows[0].bottom, vec![(0, 0), (0, 1)]);
        assert_eq!(rows[1].col, 1, "the merged branch gets its own lane");
        assert_eq!(rows[2].col, 0);
        // Both lanes converge on the common ancestor.
        assert_eq!(rows[3].col, 0);
        assert!(rows[3].top.contains(&(1, 0)));
        assert_eq!(rows[3].width(), 2);
    }
}

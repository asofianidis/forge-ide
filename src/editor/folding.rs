//! Where code can fold: by indentation, or by the ranges a language server
//! reports (`textDocument/foldingRange`).
//!
//! Indentation folding is computed on demand for the lines asked about,
//! rather than for the whole file, so large files cost nothing until a fold
//! is used: a line can fold when the next non-blank line is indented deeper,
//! and the region runs to the last line before indentation returns to its
//! level (so a closing `}` stays visible, as in other editors).

use super::Buffer;

/// How far to scan for the end of an indentation region.
const SCAN_LIMIT: usize = 200_000;

/// Regions that can fold: `(start, end)` lines, where folding hides lines
/// `start + 1..=end`.
#[derive(Debug, Clone, Copy)]
pub enum FoldSource<'a> {
    /// From the text's indentation.
    Indent,
    /// From a language server, sorted by start.
    Ranges(&'a [(usize, usize)]),
}

/// Indentation width in cells (a tab counts 4), or `None` for a blank line.
fn indent(buf: &Buffer, line: usize) -> Option<usize> {
    let text = buf.line_text(line);
    let mut width = 0;
    for c in text.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += super::view::TAB_CELLS,
            _ => return Some(width),
        }
    }
    None
}

/// The next non-blank line after `line` and its indentation.
fn next_non_blank(buf: &Buffer, line: usize) -> Option<(usize, usize)> {
    let lines = buf.line_count();
    (line + 1..lines.min(line + 1 + SCAN_LIMIT)).find_map(|l| indent(buf, l).map(|i| (l, i)))
}

impl FoldSource<'_> {
    /// Whether a region starts at `line` (cheap; for the gutter markers).
    pub fn foldable(&self, buf: &Buffer, line: usize) -> bool {
        match self {
            FoldSource::Indent => indent(buf, line)
                .is_some_and(|own| next_non_blank(buf, line).is_some_and(|(_, next)| next > own)),
            FoldSource::Ranges(r) => {
                let i = r.partition_point(|&(s, _)| s < line);
                r.get(i).is_some_and(|&(s, e)| s == line && e > s)
            }
        }
    }

    /// The region starting at `line`, if any. With several (language
    /// servers can report nested ones on one line), the largest.
    pub fn range_at(&self, buf: &Buffer, line: usize) -> Option<(usize, usize)> {
        match self {
            FoldSource::Indent => {
                let own = indent(buf, line)?;
                let (first, next) = next_non_blank(buf, line)?;
                if next <= own {
                    return None;
                }
                let mut end = first;
                let mut l = first;
                while let Some((n, i)) = next_non_blank(buf, l) {
                    if i <= own || n > line + SCAN_LIMIT {
                        break;
                    }
                    end = n;
                    l = n;
                }
                Some((line, end))
            }
            FoldSource::Ranges(r) => {
                let i = r.partition_point(|&(s, _)| s < line);
                r[i..]
                    .iter()
                    .take_while(|&&(s, _)| s == line)
                    .filter(|&&(s, e)| e > s && e < buf.line_count())
                    .max_by_key(|&&(_, e)| e)
                    .copied()
            }
        }
    }

    /// The innermost region containing `line` (starting at or above it).
    pub fn enclosing(&self, buf: &Buffer, line: usize) -> Option<(usize, usize)> {
        match self {
            FoldSource::Indent => {
                if let Some(r) = self.range_at(buf, line) {
                    return Some(r);
                }
                // A header above with less indentation than this line.
                let own = (0..=line).rev().find_map(|l| indent(buf, l)).unwrap_or(0);
                let stop = line.saturating_sub(SCAN_LIMIT);
                (stop..line)
                    .rev()
                    .filter(|&l| indent(buf, l).is_some_and(|i| i < own))
                    .find_map(|l| self.range_at(buf, l).filter(|&(_, e)| e >= line))
            }
            FoldSource::Ranges(r) => r
                .iter()
                .filter(|&&(s, e)| s <= line && line <= e && e > s)
                .min_by_key(|&&(s, e)| e - s)
                .copied(),
        }
    }

    /// Every region in the document, outermost first where they nest.
    pub fn all(&self, buf: &Buffer) -> Vec<(usize, usize)> {
        match self {
            FoldSource::Indent => (0..buf.line_count())
                .filter(|&l| self.foldable(buf, l))
                .filter_map(|l| self.range_at(buf, l))
                .collect(),
            FoldSource::Ranges(r) => r
                .iter()
                .filter(|&&(s, e)| e > s && e < buf.line_count())
                .copied()
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "fn a() {\n    if x {\n        y();\n\n    }\n    z();\n}\n\nfn b() {}\n";

    #[test]
    fn indentation_regions() {
        let buf = Buffer::from(CODE);
        let f = FoldSource::Indent;
        assert!(f.foldable(&buf, 0));
        assert!(f.foldable(&buf, 1));
        assert!(!f.foldable(&buf, 2));
        assert!(!f.foldable(&buf, 8));
        // The closing brace stays visible; blank lines inside are included.
        assert_eq!(f.range_at(&buf, 0), Some((0, 5)));
        assert_eq!(f.range_at(&buf, 1), Some((1, 2)));
        assert_eq!(f.range_at(&buf, 2), None);
        assert_eq!(f.all(&buf), vec![(0, 5), (1, 2)]);
    }

    #[test]
    fn enclosing_region() {
        let buf = Buffer::from(CODE);
        let f = FoldSource::Indent;
        assert_eq!(f.enclosing(&buf, 2), Some((1, 2)));
        assert_eq!(f.enclosing(&buf, 5), Some((0, 5)));
        assert_eq!(f.enclosing(&buf, 0), Some((0, 5)));
        assert_eq!(f.enclosing(&buf, 8), None);
    }

    #[test]
    fn server_ranges() {
        let buf = Buffer::from(CODE);
        let ranges = [(0, 5), (1, 3), (1, 2), (8, 8)];
        let f = FoldSource::Ranges(&ranges);
        assert!(f.foldable(&buf, 1));
        assert!(!f.foldable(&buf, 8), "single-line ranges don't fold");
        assert_eq!(f.range_at(&buf, 1), Some((1, 3)));
        assert_eq!(f.enclosing(&buf, 2), Some((1, 2)));
        assert_eq!(f.enclosing(&buf, 4), Some((0, 5)));
    }
}

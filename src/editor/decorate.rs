//! Layering language-server information over syntax highlighting: semantic
//! token colors, and room for inlay hints. Both are spliced into the
//! highlighted `LayoutJob` without changing its text, so character indices
//! (and the editor's cursor) still line up with the buffer.
//!
//! Server results describe the text as it was when they were asked for.
//! [`TextChange`] carries them over to the current text, so they don't
//! flicker away while the user types and the next results are on their way.

use std::ops::Range;

use egui::text::{LayoutJob, LayoutSection};
use egui::{Color32, Stroke};

/// How a semantic token restyles the text under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleOverride {
    pub color: Option<Color32>,
    pub italics: Option<bool>,
    pub strikethrough: bool,
}

/// Restyling and spacing to splice into a highlighted job.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Overlay {
    /// Byte ranges to restyle, sorted and not overlapping.
    pub styles: Vec<(Range<usize>, StyleOverride)>,
    /// Extra space (in points) in front of the char at a byte offset; sorted
    /// with unique offsets.
    pub gaps: Vec<(usize, f32)>,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty() && self.gaps.is_empty()
    }
}

/// Restyling and hint gaps for a whole document, in char offsets, so the
/// editor can cut out the part each row on screen needs.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Decorations {
    /// Char ranges to restyle, sorted and not overlapping.
    pub styles: Vec<(Range<usize>, StyleOverride)>,
    /// Extra space in front of the char at a char offset; sorted, unique.
    pub gaps: Vec<(usize, f32)>,
}

impl Decorations {
    /// The overlay for one row: `text` holds the chars `chars` of the
    /// document. Gaps at the row's end belong to the next row.
    pub fn row_overlay(&self, chars: Range<usize>, text: &str) -> Overlay {
        let mut overlay = Overlay::default();
        if chars.is_empty() {
            return overlay;
        }
        // Byte offset (in `text`) of each char boundary of the row.
        let bytes: Vec<usize> = text
            .char_indices()
            .map(|(b, _)| b)
            .chain([text.len()])
            .collect();
        let byte_of = |c: usize| bytes[(c - chars.start).min(bytes.len() - 1)];
        let first = self.styles.partition_point(|(r, _)| r.end <= chars.start);
        for (r, style) in &self.styles[first..] {
            if r.start >= chars.end {
                break;
            }
            let a = byte_of(r.start.max(chars.start));
            let b = byte_of(r.end.min(chars.end));
            overlay.styles.push((a..b, *style));
        }
        let first = self.gaps.partition_point(|&(at, _)| at < chars.start);
        for &(at, width) in &self.gaps[first..] {
            if at >= chars.end {
                break;
            }
            overlay.gaps.push((byte_of(at), width));
        }
        overlay
    }
}

/// Splits `job`'s sections where the overlay starts or stops, restyles the
/// pieces under a style and widens the ones after a gap.
pub fn apply(job: &mut LayoutJob, overlay: &Overlay) {
    if overlay.is_empty() {
        return;
    }
    let styles = &overlay.styles;
    let gaps = &overlay.gaps;
    let old = std::mem::take(&mut job.sections);
    let mut out = Vec::with_capacity(old.len() + styles.len() * 2 + gaps.len());
    let (mut si, mut gi) = (0, 0);
    for section in old {
        let range = section.byte_range.start.0..section.byte_range.end.0;
        if range.is_empty() {
            out.push(section);
            continue;
        }
        let mut start = range.start;
        let mut leading = section.leading_space;
        while start < range.end {
            while si < styles.len() && styles[si].0.end <= start {
                si += 1;
            }
            while gi < gaps.len() && gaps[gi].0 < start {
                gi += 1;
            }
            if let Some(&(at, width)) = gaps.get(gi)
                && at == start
            {
                leading += width;
                gi += 1;
            }
            let mut end = range.end;
            if let Some(&(at, _)) = gaps.get(gi) {
                end = end.min(at);
            }
            let mut format = section.format.clone();
            match styles.get(si) {
                Some((r, style)) if r.start <= start => {
                    end = end.min(r.end);
                    restyle(&mut format, style);
                }
                Some((r, _)) => end = end.min(r.start),
                None => {}
            }
            out.push(LayoutSection {
                leading_space: leading,
                byte_range: start.into()..end.into(),
                format,
            });
            leading = 0.0;
            start = end;
        }
    }
    job.sections = out;
}

fn restyle(format: &mut egui::TextFormat, style: &StyleOverride) {
    if let Some(color) = style.color {
        format.color = color;
        if format.underline != Stroke::NONE {
            format.underline.color = color;
        }
    }
    if let Some(italics) = style.italics {
        format.italics = italics;
    }
    if style.strikethrough {
        format.strikethrough = Stroke::new(1.0, format.color);
    }
}

/// TextMate scopes to color a semantic token type like, best first. Empty
/// for tokens syntax highlighting already handles well (comments, strings,
/// keywords, …), which are left alone.
pub fn semantic_scopes(kind: &str, readonly: bool, library: bool) -> &'static [&'static str] {
    match kind {
        "namespace" | "module" | "crate" => &["entity.name.namespace", "entity.name.module"],
        "type" | "typeAlias" => &["entity.name.type", "support.type"],
        "class" => &[
            "entity.name.type.class",
            "entity.name.class",
            "entity.name.type",
        ],
        "struct" | "union" => &[
            "entity.name.type.struct",
            "entity.name.struct",
            "entity.name.type",
        ],
        "enum" => &[
            "entity.name.type.enum",
            "entity.name.enum",
            "entity.name.type",
        ],
        "interface" | "trait" | "concept" => &[
            "entity.name.type.interface",
            "entity.name.trait",
            "entity.name.type",
        ],
        "typeParameter" => &["entity.name.type.parameter", "entity.name.type"],
        "builtinType" => &[
            "support.type.primitive",
            "storage.type.primitive",
            "support.type",
        ],
        "parameter" => &["variable.parameter"],
        "variable" if readonly => &["variable.other.constant", "constant.other", "variable"],
        "variable" if library => &["support.variable", "variable"],
        "variable" => &["variable.other.readwrite", "variable"],
        "property" | "field" => &[
            "variable.other.property",
            "variable.other.member",
            "variable",
        ],
        "enumMember" => &[
            "variable.other.enummember",
            "constant.other.enum",
            "constant.other",
        ],
        "event" => &["variable.other.event", "variable"],
        "const" | "static" => &["variable.other.constant", "constant.other"],
        "function" if library => &["support.function", "entity.name.function"],
        "function" => &["entity.name.function", "support.function"],
        "method" => &["entity.name.function.method", "entity.name.function"],
        "macro" => &[
            "entity.name.function.macro",
            "support.function.macro",
            "entity.name.function",
        ],
        "decorator" | "attribute" | "derive" => &[
            "entity.name.function.decorator",
            "entity.other.attribute-name",
        ],
        "label" => &["entity.name.label"],
        "lifetime" => &["storage.modifier.lifetime", "entity.name.lifetime"],
        "selfKeyword" | "selfTypeKeyword" => &["variable.language.self", "variable.language"],
        "boolean" => &["constant.language.boolean", "constant.language"],
        "escapeSequence" => &["constant.character.escape"],
        "formatSpecifier" => &["constant.other.placeholder"],
        _ => &[],
    }
}

/// How the text changed between two versions: everything before `start`
/// and after the changed region is the same in both. Offsets are chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextChange {
    pub start: usize,
    /// End of the changed region in the old text.
    pub old_end: usize,
    /// End of the changed region in the new text.
    pub new_end: usize,
}

impl TextChange {
    /// The smallest change that turns `old` into `new`, as one region.
    pub fn between(old: &str, new: &str) -> Self {
        let (a, b) = (old.as_bytes(), new.as_bytes());
        let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        while !old.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let max_suffix = a.len().min(b.len()) - prefix;
        let mut suffix = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take(max_suffix)
            .take_while(|(x, y)| x == y)
            .count();
        while !old.is_char_boundary(a.len() - suffix) {
            suffix -= 1;
        }
        let start = old[..prefix].chars().count();
        let tail = old[a.len() - suffix..].chars().count();
        Self {
            start,
            old_end: old.chars().count() - tail,
            new_end: new.chars().count() - tail,
        }
    }

    fn shift(&self, p: usize) -> usize {
        p + self.new_end - self.old_end
    }

    /// Where a range of untouched text went, or `None` if it was edited.
    pub fn map_range(&self, r: &Range<usize>) -> Option<Range<usize>> {
        if r.end <= self.start {
            Some(r.clone())
        } else if r.start >= self.old_end {
            Some(self.shift(r.start)..self.shift(r.end))
        } else {
            None
        }
    }

    /// Where a position went, or `None` if it was at or inside the change
    /// (it is unclear which side it belongs to).
    pub fn map_point(&self, p: usize) -> Option<usize> {
        if p < self.start {
            Some(p)
        } else if p > self.old_end {
            Some(self.shift(p))
        } else {
            None
        }
    }

    /// Where a range went, stretching or shrinking it if the change was
    /// inside it (e.g. a function whose body was edited).
    pub fn stretch_range(&self, r: &Range<usize>) -> Range<usize> {
        let map = |p: usize, is_end: bool| {
            if p <= self.start {
                p
            } else if p >= self.old_end {
                self.shift(p)
            } else if is_end {
                self.new_end
            } else {
                self.start
            }
        };
        let start = map(r.start, false);
        start..map(r.end, true).max(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::FontId;
    use egui::text::TextFormat;

    fn job(pieces: &[(&str, Color32)]) -> LayoutJob {
        let mut job = LayoutJob::default();
        for (text, color) in pieces {
            job.append(
                text,
                0.0,
                TextFormat::simple(FontId::monospace(12.0), *color),
            );
        }
        job
    }

    /// (text, color, leading space) of every section.
    fn sections(job: &LayoutJob) -> Vec<(&str, Color32, f32)> {
        job.sections
            .iter()
            .map(|s| {
                (
                    &job.text[s.byte_range.start.0..s.byte_range.end.0],
                    s.format.color,
                    s.leading_space,
                )
            })
            .collect()
    }

    fn color(c: Color32) -> StyleOverride {
        StyleOverride {
            color: Some(c),
            italics: None,
            strikethrough: false,
        }
    }

    #[test]
    fn splits_sections_for_styles_and_gaps() {
        let (w, r, g) = (Color32::WHITE, Color32::RED, Color32::GREEN);
        // "let value = add(x);" highlighted as two sections.
        let mut j = job(&[("let value = ", w), ("add(x);", w)]);
        let overlay = Overlay {
            // `value` and `add` restyled; the second crosses no boundary.
            styles: vec![(4..9, color(r)), (12..15, color(g))],
            // Room for `: i32` after `value`, and `n: ` before `x`.
            gaps: vec![(9, 30.0), (16, 18.0)],
        };
        apply(&mut j, &overlay);
        assert_eq!(
            sections(&j),
            [
                ("let ", w, 0.0),
                ("value", r, 0.0),
                (" = ", w, 30.0),
                ("add", g, 0.0),
                ("(", w, 0.0),
                ("x);", w, 18.0),
            ]
        );
        assert_eq!(j.text, "let value = add(x);");
    }

    #[test]
    fn styles_may_span_sections_and_gaps_may_start_one() {
        let (w, r) = (Color32::WHITE, Color32::RED);
        let mut j = job(&[("ab", w), ("cd", w)]);
        let overlay = Overlay {
            styles: vec![(1..3, color(r))],
            gaps: vec![(0, 5.0), (2, 7.0)],
        };
        apply(&mut j, &overlay);
        assert_eq!(
            sections(&j),
            [("a", w, 5.0), ("b", r, 0.0), ("c", r, 7.0), ("d", w, 0.0)]
        );
    }

    #[test]
    fn empty_overlay_leaves_job_alone() {
        let mut j = job(&[("x", Color32::WHITE)]);
        let before = j.clone();
        apply(&mut j, &Overlay::default());
        assert_eq!(j, before);
    }

    #[test]
    fn cuts_document_decorations_per_row() {
        let style = color(Color32::RED);
        let d = Decorations {
            // `é` is two bytes; the second style crosses into the next row.
            styles: vec![(1..3, style), (5..9, style)],
            gaps: vec![(2, 10.0), (6, 20.0), (8, 5.0)],
        };
        // Row "aébcde" holds chars 0..6.
        let row = d.row_overlay(0..6, "aébcde");
        assert_eq!(row.styles, vec![(1..4, style), (6..7, style)]);
        assert_eq!(
            row.gaps,
            vec![(3, 10.0)],
            "a gap at the row's end goes to the next row"
        );
        // The next row, chars 6..9 = "fgh".
        let row = d.row_overlay(6..9, "fgh");
        assert_eq!(row.styles, vec![(0..3, style)]);
        assert_eq!(row.gaps, vec![(0, 20.0), (2, 5.0)]);
    }

    #[test]
    fn finds_the_changed_region() {
        let c = TextChange::between("let x = 1;", "let xy = 1;");
        assert_eq!((c.start, c.old_end, c.new_end), (5, 5, 6));
        let c = TextChange::between("abc", "abc");
        assert_eq!((c.start, c.old_end, c.new_end), (3, 3, 3));
        // Multi-byte chars: offsets are in chars, never split a char.
        let c = TextChange::between("é = 1", "è = 1");
        assert_eq!((c.start, c.old_end, c.new_end), (0, 1, 1));
        let c = TextChange::between("aéb", "ab");
        assert_eq!((c.start, c.old_end, c.new_end), (1, 2, 1));
        // Repeated text: the change is placed as early as the prefix allows.
        let c = TextChange::between("aaa", "aaaa");
        assert_eq!((c.start, c.old_end, c.new_end), (3, 3, 4));
    }

    #[test]
    fn maps_ranges_and_points_across_a_change() {
        // "foo(bar)" -> "foo(bazz)": `bar` was edited into `bazz`.
        let c = TextChange::between("foo(bar) x", "foo(bazz) x");
        assert_eq!(c.map_range(&(0..3)), Some(0..3));
        assert_eq!(c.map_range(&(4..7)), None);
        assert_eq!(c.map_range(&(9..10)), Some(10..11));
        assert_eq!(c.map_point(1), Some(1));
        assert_eq!(c.map_point(9), Some(10));
        assert_eq!(c.map_point(6), None);
        // A function around the edit grows with it.
        assert_eq!(c.stretch_range(&(0..8)), 0..9);
        // A range starting inside the edit begins where it does.
        assert_eq!(c.stretch_range(&(6..10)), 6..11);
    }
}

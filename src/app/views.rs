//! What the editor keeps showing from the language server for each open
//! file: semantic highlighting, inlay hints and the symbols behind the
//! breadcrumbs. Results are carried across edits until fresh ones arrive.

use std::ops::Range;
use std::sync::Arc;

use egui::text::CCursor;
use egui::text_edit::TextEditOutput;
use egui::{Color32, FontId, Galley, Rect, pos2, vec2};

use crate::editor::Highlighter;
use crate::editor::decorate::{Overlay, TextChange};
use crate::lsp::features::{DocSymbol, Hint, SemanticSpan};

/// Space around a hint's text inside its pill, and between pills and code.
const HINT_PAD: f32 = 3.0;
const HINT_MARGIN: f32 = 4.0;

#[derive(Default)]
pub(super) struct DocViews {
    /// The text the ranges below refer to.
    text: String,
    tokens: Vec<SemanticSpan>,
    hints: Vec<Hint>,
    /// `None` until the server has sent them.
    pub(super) symbols: Option<Vec<DocSymbol>>,
    /// Bumped whenever the ranges change, to rebuild `prepared`.
    revision: u64,
    prepared: Option<Prepared>,
}

/// The views turned into layout changes for one text, font and theme.
struct Prepared {
    key: (u64, u64, FontId, bool),
    overlay: Overlay,
    hints: Vec<PlacedHint>,
}

/// An inlay hint laid out for painting.
pub(super) struct PlacedHint {
    /// The char the hint sits in front of.
    at: usize,
    /// After the end of its line, rather than in a gap made in the text.
    eol: bool,
    /// From the start of the gap (or the line end) to this hint's pill.
    x: f32,
    /// Width of the gap made for all hints at `at`.
    gap: f32,
    galley: Arc<Galley>,
}

impl DocViews {
    /// Carries the ranges over to `text`, dropping those that were edited.
    fn rebase(&mut self, text: &str) {
        if self.text == text {
            return;
        }
        let change = TextChange::between(&self.text, text);
        self.tokens
            .retain_mut(|t| match change.map_range(&t.range) {
                Some(r) => {
                    t.range = r;
                    true
                }
                None => false,
            });
        self.hints.retain_mut(|h| match change.map_point(h.at) {
            Some(at) => {
                h.at = at;
                true
            }
            None => false,
        });
        if let Some(symbols) = &mut self.symbols {
            stretch_symbols(symbols, &change);
        }
        self.text.clear();
        self.text.push_str(text);
        self.revision += 1;
    }

    pub(super) fn set_tokens(&mut self, text: &str, tokens: Vec<SemanticSpan>) {
        self.rebase(text);
        self.tokens = tokens;
        self.revision += 1;
    }

    pub(super) fn set_hints(&mut self, text: &str, hints: Vec<Hint>) {
        self.rebase(text);
        self.hints = hints;
        self.revision += 1;
    }

    pub(super) fn set_symbols(&mut self, text: &str, symbols: Vec<DocSymbol>) {
        self.rebase(text);
        self.symbols = Some(symbols);
        self.revision += 1;
    }

    /// The symbols, placed in `text`.
    pub(super) fn symbols_for(&mut self, text: &str) -> Option<&[DocSymbol]> {
        self.symbols.as_ref()?;
        self.rebase(text);
        self.symbols.as_deref()
    }

    /// Whether the symbols were computed for exactly `text`.
    pub(super) fn symbols_current(&self, text: &str) -> bool {
        self.symbols.is_some() && self.text == text
    }

    /// Restyling and hint gaps for `text`. Called from the editor's layouter
    /// with whatever text it is laying out.
    pub(super) fn overlay(
        &mut self,
        ui: &egui::Ui,
        text: &str,
        highlighter: &mut Highlighter,
        font: &FontId,
        hints: Option<Color32>,
    ) -> &Overlay {
        self.rebase(text);
        let key = (
            self.revision,
            highlighter.generation(),
            font.clone(),
            hints.is_some(),
        );
        if self.prepared.as_ref().is_none_or(|p| p.key != key) {
            let mut bytes = ByteCursor::new(&self.text);
            let mut overlay = Overlay::default();
            let mut covered = 0;
            for t in &self.tokens {
                // Tokens shouldn't overlap; if a server sends some anyway,
                // the first one wins.
                if t.range.start < covered {
                    continue;
                }
                covered = t.range.end;
                if let Some(style) = highlighter.semantic_style(&t.kind, t.modifiers) {
                    let r = bytes.byte_of(t.range.start)..bytes.byte_of(t.range.end);
                    overlay.styles.push((r, style));
                }
            }
            let placed = match hints {
                Some(color) => self.place_hints(ui, font, color, &mut overlay),
                None => Vec::new(),
            };
            self.prepared = Some(Prepared {
                key,
                overlay,
                hints: placed,
            });
        }
        &self.prepared.as_ref().expect("prepared above").overlay
    }

    /// Lays out the hints and makes room for the ones inside a line.
    fn place_hints(
        &self,
        ui: &egui::Ui,
        font: &FontId,
        color: Color32,
        overlay: &mut Overlay,
    ) -> Vec<PlacedHint> {
        let hint_font = FontId::new((font.size * 0.9).round(), font.family.clone());
        let mut bytes = ByteCursor::new(&self.text);
        let mut placed: Vec<PlacedHint> = Vec::with_capacity(self.hints.len());
        let mut i = 0;
        while i < self.hints.len() {
            let at = self.hints[i].at;
            let group_start = placed.len();
            let mut x = 0.0;
            while let Some(h) = self.hints.get(i).filter(|h| h.at == at) {
                if h.pad_left {
                    x += HINT_MARGIN;
                }
                let galley = ui
                    .painter()
                    .layout_no_wrap(h.label.clone(), hint_font.clone(), color);
                let width = galley.size().x + 2.0 * HINT_PAD;
                placed.push(PlacedHint {
                    at,
                    eol: false,
                    x,
                    gap: 0.0,
                    galley,
                });
                x += width;
                if h.pad_right {
                    x += HINT_MARGIN;
                }
                i += 1;
            }
            let byte = bytes.byte_of(at);
            let eol = self.text[byte..].chars().next().is_none_or(|c| c == '\n');
            // Hints inside a line get a gap in the text, with a little air
            // on each side; ones at the end of a line are drawn after it.
            let gap = x + HINT_MARGIN;
            for p in &mut placed[group_start..] {
                p.eol = eol;
                p.gap = gap;
                if eol {
                    p.x += HINT_MARGIN;
                }
            }
            if !eol {
                overlay.gaps.push((byte, gap));
            }
        }
        placed
    }

    /// Hints as laid out by the last [`DocViews::overlay`] call.
    pub(super) fn placed_hints(&self) -> &[PlacedHint] {
        self.prepared.as_ref().map_or(&[], |p| &p.hints)
    }
}

fn stretch_symbols(symbols: &mut [DocSymbol], change: &TextChange) {
    for s in symbols {
        s.range = change.stretch_range(&s.range);
        s.selection = change.stretch_range(&s.selection);
        stretch_symbols(&mut s.children, change);
    }
}

/// Byte offsets of chars, for lookups in increasing order.
struct ByteCursor<'a> {
    text: &'a str,
    char: usize,
    byte: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            char: 0,
            byte: 0,
        }
    }

    fn byte_of(&mut self, char: usize) -> usize {
        if char < self.char {
            self.char = 0;
            self.byte = 0;
        }
        for c in self.text[self.byte..].chars().take(char - self.char) {
            self.byte += c.len_utf8();
            self.char += 1;
        }
        self.byte
    }
}

/// Draws inlay hints as small pills in the gaps made for them (or after the
/// end of their line).
pub(super) fn paint_hints(
    ui: &egui::Ui,
    output: &TextEditOutput,
    hints: &[PlacedHint],
    visible: &Range<usize>,
    background: Color32,
) {
    let offset = output.galley_pos.to_vec2();
    let galley = &output.galley;
    let left = output.galley_pos.x;
    let painter = ui.painter();
    let first = hints.partition_point(|h| h.at < visible.start);
    for h in &hints[first..] {
        if h.at > visible.end {
            break;
        }
        let pos = galley.pos_from_cursor(CCursor::new(h.at)).translate(offset);
        let start = if h.eol {
            pos.left()
        } else {
            (pos.left() - h.gap).max(left)
        };
        let size = h.galley.size();
        let rect = Rect::from_min_size(
            pos2(start + h.x, pos.center().y - size.y / 2.0 - 1.0),
            vec2(size.x + 2.0 * HINT_PAD, size.y + 2.0),
        );
        painter.rect_filled(rect, 3.0, background);
        painter.galley(
            pos2(rect.left() + HINT_PAD, rect.top() + 1.0),
            Arc::clone(&h.galley),
            Color32::PLACEHOLDER,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::features::HintKind;
    use lsp_types::SymbolKind;

    fn span(range: Range<usize>) -> SemanticSpan {
        SemanticSpan {
            range,
            kind: Arc::from("function"),
            modifiers: 0,
        }
    }

    fn hint(at: usize) -> Hint {
        Hint {
            at,
            label: ": i32".into(),
            kind: HintKind::Type,
            pad_left: false,
            pad_right: false,
            tooltip: None,
        }
    }

    #[test]
    fn carries_views_across_edits() {
        let mut v = DocViews::default();
        let old = "let a = f(b);\nfn f() {}\n";
        v.set_tokens(old, vec![span(8..9), span(17..18)]);
        // In front of `a`, and in front of the `)` the edit types before.
        v.set_hints(old, vec![hint(5), hint(11)]);
        v.set_symbols(
            old,
            vec![DocSymbol {
                name: "f".into(),
                detail: None,
                kind: SymbolKind::FUNCTION,
                range: 14..23,
                selection: 17..18,
                children: Vec::new(),
            }],
        );
        // Edit the argument: `b` -> `bc`.
        let new = "let a = f(bc);\nfn f() {}\n";
        let symbols = v.symbols_for(new).unwrap();
        assert_eq!(symbols[0].range, 15..24);
        assert_eq!(symbols[0].selection, 18..19);
        let ranges: Vec<_> = v.tokens.iter().map(|t| t.range.clone()).collect();
        assert_eq!(ranges, [8..9, 18..19]);
        // The hint at the edit is dropped until the server sends new ones.
        let hints: Vec<_> = v.hints.iter().map(|h| h.at).collect();
        assert_eq!(hints, [5]);
        assert!(v.symbols_current(new));
        assert!(!v.symbols_current(old));
    }

    #[test]
    fn byte_cursor_handles_multibyte_and_rewinds() {
        let mut b = ByteCursor::new("aé€b");
        assert_eq!(b.byte_of(1), 1);
        assert_eq!(b.byte_of(3), 6);
        assert_eq!(b.byte_of(2), 3);
        assert_eq!(b.byte_of(4), 7);
    }
}

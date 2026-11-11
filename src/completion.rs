//! Completion popup fed by the language server.

use egui::{Key, Modifiers};
use lsp_types::{CompletionItem, CompletionTextEdit, InsertTextFormat};

use crate::editor::Document;
use crate::lsp::{char_range, strip_snippet};
use crate::palette::fuzzy_match;

/// Most items rendered at once; the rest are reachable by filtering.
const MAX_VISIBLE: usize = 200;

pub enum CompletionKey {
    Accept,
    Dismiss,
}

pub struct CompletionPopup {
    /// The document the popup belongs to.
    pub doc: egui::Id,
    /// Char offset where the word being completed starts.
    pub anchor: usize,
    items: Vec<CompletionItem>,
    /// Indices into `items` matching the current prefix, best first.
    filtered: Vec<usize>,
    selected: usize,
    scroll_to_selected: bool,
}

impl CompletionPopup {
    pub fn new(doc: egui::Id, anchor: usize, mut items: Vec<CompletionItem>) -> Self {
        // Server order is by `sort_text` (falling back to the label).
        items.sort_by(|a, b| {
            let key = |i: &CompletionItem| i.sort_text.clone().unwrap_or_else(|| i.label.clone());
            key(a).cmp(&key(b))
        });
        let filtered = (0..items.len()).collect();
        Self {
            doc,
            anchor,
            items,
            filtered,
            selected: 0,
            scroll_to_selected: false,
        }
    }

    /// Narrows the list to items matching `prefix` (the text typed since `anchor`).
    pub fn set_prefix(&mut self, prefix: &str) {
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let text = item.filter_text.as_deref().unwrap_or(&item.label);
                fuzzy_match(prefix, text).map(|(score, _)| (score, i))
            })
            .collect();
        if !prefix.is_empty() {
            // Stable, so equal scores keep the server's order.
            scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        }
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.filtered.get(self.selected).map(|&i| &self.items[i])
    }

    /// Consumes navigation keys before the editor sees them.
    pub fn handle_keys(&mut self, ctx: &egui::Context) -> Option<CompletionKey> {
        ctx.input_mut(|i| {
            let len = self.filtered.len().clamp(1, MAX_VISIBLE);
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                self.selected = (self.selected + 1) % len;
                self.scroll_to_selected = true;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                self.selected = self.selected.checked_sub(1).unwrap_or(len - 1);
                self.scroll_to_selected = true;
            }
            if i.consume_key(Modifiers::NONE, Key::Enter)
                || i.consume_key(Modifiers::NONE, Key::Tab)
            {
                return Some(CompletionKey::Accept);
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                return Some(CompletionKey::Dismiss);
            }
            None
        })
    }

    /// Draws the popup with its top-left at `pos`. Returns true if an item was clicked.
    pub fn ui(&mut self, ctx: &egui::Context, pos: egui::Pos2) -> bool {
        let mut clicked = false;
        egui::Area::new(egui::Id::unique("completion_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(320.0);
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for (row, &i) in self.filtered.iter().take(MAX_VISIBLE).enumerate() {
                                let item = &self.items[i];
                                let selected = row == self.selected;
                                let response = ui
                                    .horizontal(|ui| {
                                        let r = ui.selectable_label(selected, &item.label);
                                        if let Some(detail) = item.detail.as_deref().or_else(|| {
                                            item.label_details
                                                .as_ref()
                                                .and_then(|d| d.description.as_deref())
                                        }) {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(detail).weak(),
                                                )
                                                .truncate(),
                                            );
                                        }
                                        r
                                    })
                                    .inner;
                                if selected && std::mem::take(&mut self.scroll_to_selected) {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    self.selected = row;
                                    clicked = true;
                                }
                            }
                        });
                });
            });
        clicked
    }
}

/// Inserts a completion item, replacing the word from `anchor` to `cursor`.
/// Applies any additional edits (e.g. auto-imports) in the same undo step.
/// Returns the new cursor position.
pub fn apply(
    doc: &mut Document,
    item: &CompletionItem,
    anchor: usize,
    cursor: usize,
) -> std::ops::Range<usize> {
    let (range, text) = match &item.text_edit {
        Some(CompletionTextEdit::Edit(e)) => (char_range(doc, &e.range), e.new_text.clone()),
        Some(CompletionTextEdit::InsertAndReplace(e)) => {
            (char_range(doc, &e.replace), e.new_text.clone())
        }
        None => (
            anchor..cursor,
            item.insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone()),
        ),
    };
    // The server computed the range when the request was made; also replace
    // anything typed since.
    let range = range.start..range.end.max(cursor);
    let (text, selection) = if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
        strip_snippet(&text)
    } else {
        let n = text.chars().count();
        (text, n..n)
    };
    let mut edits = vec![(range.clone(), text)];
    for extra in item.additional_text_edits.iter().flatten() {
        let r = char_range(doc, &extra.range);
        // Ignore edits that would overlap the main one.
        if r.end <= range.start || r.start >= range.end {
            edits.push((r, extra.new_text.clone()));
        }
    }
    let start = doc.buffer.replace_ranges(edits)[0].start;
    start + selection.start..start + selection.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::TextBuffer as _;
    use lsp_types::{Position, Range, TextEdit};

    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            ..Default::default()
        }
    }

    fn doc(text: &str) -> Document {
        let mut doc = Document::untitled();
        doc.buffer.insert_text(text, egui::text::CharIndex(0));
        doc
    }

    fn range(l1: u32, c1: u32, l2: u32, c2: u32) -> Range {
        Range::new(Position::new(l1, c1), Position::new(l2, c2))
    }

    #[test]
    fn filters_and_ranks_by_prefix() {
        let mut popup = CompletionPopup::new(
            egui::Id::NULL,
            0,
            vec![
                item("to_string"),
                item("len"),
                item("to_owned"),
                item("iter"),
            ],
        );
        assert_eq!(popup.filtered.len(), 4);
        popup.set_prefix("to");
        let labels: Vec<_> = popup
            .filtered
            .iter()
            .map(|&i| popup.items[i].label.as_str())
            .collect();
        assert_eq!(labels, ["to_owned", "to_string"]);
        popup.set_prefix("xyz");
        assert!(popup.is_empty());
    }

    #[test]
    fn plain_item_replaces_typed_prefix() {
        let mut d = doc("let x = fo");
        let selection = apply(&mut d, &item("foobar"), 8, 10);
        assert_eq!(d.buffer.as_str(), "let x = foobar");
        assert_eq!(selection, 14..14);
    }

    #[test]
    fn snippet_text_edit_with_extra_typing_and_import() {
        let mut d = doc("fn main() {\n    Has\n}");
        let completion = CompletionItem {
            label: "HashMap".into(),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            // Range from when "Ha" had been typed; "s" was typed since.
            text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
                range(1, 4, 1, 6),
                "HashMap::with_capacity(${1:n})$0".into(),
            ))),
            additional_text_edits: Some(vec![TextEdit::new(
                range(0, 0, 0, 0),
                "use std::collections::HashMap;\n".into(),
            )]),
            ..Default::default()
        };
        let selection = apply(&mut d, &completion, 16, 19);
        let expected =
            "use std::collections::HashMap;\nfn main() {\n    HashMap::with_capacity(n)\n}";
        assert_eq!(d.buffer.as_str(), expected);
        let selected = d
            .buffer
            .char_range(selection.start.into()..selection.end.into());
        assert_eq!(selected, "n", "the first placeholder is selected");
        // One undo step reverts both edits.
        d.buffer.undo();
        assert_eq!(d.buffer.as_str(), "fn main() {\n    Has\n}");
    }
}

//! Symbols from the language server: Go to Symbol in File, Go to Symbol in
//! Workspace, and the breadcrumb bar above the editor.

use std::ops::Range;
use std::path::Path;

use egui::RichText;

use super::ForgeApp;
use super::pickers::PickerMode;
use crate::editor::ops::Selection;
use crate::fuzzy;
use crate::lsp::features::{DocSymbol, SymbolLocation, symbol_kind_name, symbol_path};
use crate::palette::{MAX_ROWS, PickerItem};

/// A row of the Go to Symbol in File picker.
pub(super) struct SymbolItem {
    name: String,
    /// Enclosing symbols, outermost first.
    container: String,
    kind: &'static str,
    depth: usize,
    /// The symbol's name in the text, selected when it is chosen.
    selection: Range<usize>,
}

/// The Go to Symbol in Workspace search: results for the last query sent.
#[derive(Default)]
pub(super) struct WorkspaceSearch {
    /// The query the results are for, once it has been sent.
    query: Option<String>,
    /// Servers asked, and how many have answered.
    asked: usize,
    answered: usize,
    results: Vec<SymbolLocation>,
}

/// One symbol in the breadcrumb bar, with the symbols beside it.
struct Crumb {
    name: String,
    siblings: Vec<(String, &'static str, Range<usize>)>,
    current: usize,
}

impl ForgeApp {
    // ---- in the file -----------------------------------------------------

    /// Cmd/Ctrl+Shift+O: opens the symbol picker for the active file, asking
    /// the server first unless the symbols are up to date.
    pub(super) fn go_to_symbol(&mut self) {
        self.active_pane = None;
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let Some(path) = doc.path.clone() else {
            self.status = "Save the file to use language features".to_owned();
            return;
        };
        if self
            .language
            .views
            .get(&path)
            .is_some_and(|v| v.symbols_current(doc.text()))
        {
            self.open_symbol_picker(&path);
            return;
        }
        let text = doc.text().to_owned();
        if let Err(e) = self.lsp.document_symbols(&path, &text) {
            self.status = e;
        }
    }

    /// Shows the active file's symbols in the picker, if it is `path`.
    pub(super) fn open_symbol_picker(&mut self, path: &Path) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        if doc.path.as_deref() != Some(path) {
            return;
        }
        let Some(symbols) = self
            .language
            .views
            .get_mut(path)
            .and_then(|v| v.symbols_for(doc.text()))
        else {
            return;
        };
        let mut items = Vec::new();
        flatten(symbols, 0, "", &mut items);
        if items.is_empty() {
            self.status = "No symbols in this file".to_owned();
            return;
        }
        self.language.symbol_items = items;
        self.go_to_line = None;
        self.picker_mode = PickerMode::Symbols;
        self.picker.open("");
    }

    /// Picker rows for the file's symbols, with the index of each. In file
    /// order (indented by nesting) until a query is typed, then best first.
    pub(super) fn symbol_picker_items(&self, query: &str) -> (Vec<PickerItem>, Vec<usize>) {
        let items = &self.language.symbol_items;
        if query.trim().is_empty() {
            return items
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let item = PickerItem {
                        label: format!("{}{}", "  ".repeat(s.depth), s.name),
                        detail: s.kind.to_owned(),
                        highlights: Vec::new(),
                    };
                    (item, i)
                })
                .unzip();
        }
        let mut ranked: Vec<(i32, usize, Vec<usize>)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, s)| fuzzy::score(query, &s.name).map(|m| (m.score, i, m.indices)))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        ranked
            .into_iter()
            .map(|(_, i, highlights)| {
                let s = &items[i];
                let detail = if s.container.is_empty() {
                    s.kind.to_owned()
                } else {
                    format!("{} · {}", s.kind, s.container)
                };
                let item = PickerItem {
                    label: s.name.clone(),
                    detail,
                    highlights,
                };
                (item, i)
            })
            .unzip()
    }

    pub(super) fn go_to_symbol_item(&mut self, ctx: &egui::Context, idx: usize) {
        if let Some(s) = self.language.symbol_items.get(idx) {
            let sel = Selection::new(s.selection.start, s.selection.end);
            self.set_selection(ctx, sel, true);
        }
    }

    // ---- in the workspace ------------------------------------------------

    /// Cmd/Ctrl+T: searches every running server's symbols as you type.
    pub(super) fn go_to_workspace_symbol(&mut self) {
        self.language.workspace_search = WorkspaceSearch::default();
        self.go_to_line = None;
        self.picker_mode = PickerMode::WorkspaceSymbols;
        self.picker.open("");
    }

    /// Sends the picker's query if it changed, and returns its rows, the
    /// index of each result, and a footer line.
    pub(super) fn workspace_symbol_items(
        &mut self,
    ) -> (Vec<PickerItem>, Vec<usize>, Option<String>) {
        let query = self.picker.query.trim().to_owned();
        let search = &mut self.language.workspace_search;
        if search.query.as_deref() != Some(query.as_str()) {
            let asked = self.lsp.workspace_symbols(&query);
            *search = WorkspaceSearch {
                query: Some(query.clone()),
                asked,
                answered: 0,
                results: Vec::new(),
            };
        }
        let search = &self.language.workspace_search;
        let mut ranked: Vec<(i32, usize, Vec<usize>)> = search
            .results
            .iter()
            .enumerate()
            .map(|(i, s)| match fuzzy::score(&query, &s.name) {
                Some(m) => (m.score, i, m.indices),
                // The server matched it some other way (e.g. by container).
                None => (i32::MIN, i, Vec::new()),
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        ranked.truncate(MAX_ROWS);
        let (items, indices) = ranked
            .into_iter()
            .map(|(_, i, highlights)| {
                let s = &search.results[i];
                let mut detail = format!(
                    "{}  {}:{}",
                    symbol_kind_name(s.kind),
                    self.display_path(&s.location.path),
                    s.location.range.start.line + 1
                );
                if let Some(c) = &s.container {
                    detail = format!("{c} · {detail}");
                }
                let item = PickerItem {
                    label: s.name.clone(),
                    detail,
                    highlights,
                };
                (item, i)
            })
            .unzip();
        let footer = if search.asked == 0 {
            Some("No running language server can search the workspace".to_owned())
        } else if search.answered < search.asked {
            Some("Searching…".to_owned())
        } else if search.results.is_empty() && query.is_empty() {
            Some("Type to search for symbols".to_owned())
        } else {
            None
        };
        (items, indices, footer)
    }

    pub(super) fn on_workspace_symbols(&mut self, query: String, symbols: Vec<SymbolLocation>) {
        let search = &mut self.language.workspace_search;
        if search.query.as_ref() == Some(&query) {
            search.answered += 1;
            search.results.extend(symbols);
        }
    }

    pub(super) fn go_to_workspace_symbol_item(&mut self, ctx: &egui::Context, idx: usize) {
        let Some(location) = self
            .language
            .workspace_search
            .results
            .get(idx)
            .map(|s| s.location.clone())
        else {
            return;
        };
        self.go_to_location(ctx, &location);
    }

    // ---- breadcrumbs -----------------------------------------------------

    /// The file's path and the symbols around the caret, above the editor.
    /// Clicking a symbol lists the ones beside it to jump to.
    pub(super) fn breadcrumbs(&mut self, ui: &mut egui::Ui) {
        if !self.settings.breadcrumbs {
            return;
        }
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let Some(path) = doc.path.clone() else {
            return;
        };
        let caret = self.selection(ui.ctx()).end;
        let segments: Vec<String> = Path::new(&self.display_path(&path))
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let crumbs = self
            .language
            .views
            .get_mut(&path)
            .and_then(|v| v.symbols_for(doc.text()))
            .map(|symbols| crumbs(symbols, caret))
            .unwrap_or_default();

        let mut jump = None;
        let muted = self.current_theme().ui.text_muted.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let last = segments.len().saturating_sub(1);
            for (i, segment) in segments.iter().enumerate() {
                let text = RichText::new(segment).small();
                if i == last {
                    ui.label(text);
                } else {
                    ui.label(text.color(muted));
                }
                if i < last || !crumbs.is_empty() {
                    ui.label(RichText::new("›").small().color(muted));
                }
            }
            for (i, crumb) in crumbs.iter().enumerate() {
                ui.menu_button(RichText::new(&crumb.name).small(), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (j, (name, kind, range)) in crumb.siblings.iter().enumerate() {
                                let label = RichText::new(format!("{name}  ")).small();
                                let button = egui::Button::selectable(j == crumb.current, label)
                                    .right_text(RichText::new(*kind).small().color(muted));
                                if ui.add(button).clicked() {
                                    jump = Some(range.clone());
                                    ui.close();
                                }
                            }
                        });
                });
                if i + 1 < crumbs.len() {
                    ui.label(RichText::new("›").small().color(muted));
                }
            }
        });
        if let Some(r) = jump {
            self.set_selection(ui.ctx(), Selection::new(r.start, r.end), true);
        }
    }
}

/// Depth-first rows for the picker.
fn flatten(symbols: &[DocSymbol], depth: usize, container: &str, out: &mut Vec<SymbolItem>) {
    for s in symbols {
        out.push(SymbolItem {
            name: s.name.clone(),
            container: container.to_owned(),
            kind: symbol_kind_name(s.kind),
            depth,
            selection: s.selection.clone(),
        });
        let inner = if container.is_empty() {
            s.name.clone()
        } else {
            format!("{container} › {}", s.name)
        };
        flatten(&s.children, depth + 1, &inner, out);
    }
}

/// The chain of symbols around `at`, each with its siblings.
fn crumbs(symbols: &[DocSymbol], at: usize) -> Vec<Crumb> {
    let path = symbol_path(symbols, at);
    let mut level = symbols;
    let mut out = Vec::with_capacity(path.len());
    for s in path {
        out.push(Crumb {
            name: s.name.clone(),
            siblings: level
                .iter()
                .map(|x| {
                    (
                        x.name.clone(),
                        symbol_kind_name(x.kind),
                        x.selection.clone(),
                    )
                })
                .collect(),
            current: level.iter().position(|x| std::ptr::eq(x, s)).unwrap_or(0),
        });
        level = &s.children;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::SymbolKind;

    fn sym(name: &str, range: Range<usize>, children: Vec<DocSymbol>) -> DocSymbol {
        DocSymbol {
            name: name.into(),
            detail: None,
            kind: SymbolKind::FUNCTION,
            selection: range.start..range.start + name.len(),
            range,
            children,
        }
    }

    #[test]
    fn flattens_with_depth_and_container() {
        let symbols = vec![
            sym(
                "A",
                0..50,
                vec![sym("a1", 5..20, vec![sym("x", 6..8, vec![])])],
            ),
            sym("B", 60..70, vec![]),
        ];
        let mut items = Vec::new();
        flatten(&symbols, 0, "", &mut items);
        let got: Vec<_> = items
            .iter()
            .map(|i| (i.name.as_str(), i.depth, i.container.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("A", 0, ""),
                ("a1", 1, "A"),
                ("x", 2, "A › a1"),
                ("B", 0, "")
            ]
        );
    }

    #[test]
    fn crumbs_list_siblings_at_each_level() {
        let symbols = vec![
            sym(
                "A",
                0..50,
                vec![sym("a1", 5..20, vec![]), sym("a2", 25..40, vec![])],
            ),
            sym("B", 60..70, vec![]),
        ];
        let c = crumbs(&symbols, 30);
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].name.as_str(), c[0].current, c[0].siblings.len()),
            ("A", 0, 2)
        );
        assert_eq!((c[1].name.as_str(), c[1].current), ("a2", 1));
        assert_eq!(c[1].siblings[0].2, 5..7);
        assert!(crumbs(&symbols, 55).is_empty());
    }
}

//! Decoding the replies behind the richer language views: semantic tokens,
//! inlay hints, document and workspace symbols, and signature help. Ranges
//! come back as character indices into the text the request was made for.

use std::ops::Range;
use std::sync::Arc;

use lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, InlayHint, InlayHintKind, InlayHintLabel,
    InlayHintTooltip, OneOf, ParameterLabel, SemanticTokensLegend, SymbolInformation, SymbolKind,
    WorkspaceSymbolResponse,
};

use super::session::Location;
use super::text::{Encoding, LineIndex, uri_to_path};

/// Token modifiers the editor styles; others are ignored.
pub mod token_mods {
    pub const READONLY: u8 = 1;
    pub const DEPRECATED: u8 = 2;
    pub const DEFAULT_LIBRARY: u8 = 4;
    pub const STATIC: u8 = 8;
}

/// A run of text the server classified, e.g. a parameter or a method name.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSpan {
    pub range: Range<usize>,
    /// The token type, e.g. `function` or `enumMember`.
    pub kind: Arc<str>,
    /// [`token_mods`] flags.
    pub modifiers: u8,
}

/// Decodes the protocol's packed, relative token list.
pub fn decode_semantic_tokens(
    text: &str,
    data: &[lsp_types::SemanticToken],
    legend: &SemanticTokensLegend,
    enc: Encoding,
) -> Vec<SemanticSpan> {
    let kinds: Vec<Arc<str>> = legend
        .token_types
        .iter()
        .map(|t| Arc::from(t.as_str()))
        .collect();
    let modifier_flags: Vec<u8> = legend
        .token_modifiers
        .iter()
        .map(|m| match m.as_str() {
            "readonly" => token_mods::READONLY,
            "deprecated" => token_mods::DEPRECATED,
            "defaultLibrary" => token_mods::DEFAULT_LIBRARY,
            "static" => token_mods::STATIC,
            _ => 0,
        })
        .collect();
    let index = LineIndex::new(text);
    let mut spans = Vec::with_capacity(data.len());
    let (mut line, mut start) = (0u32, 0u32);
    for t in data {
        if t.delta_line > 0 {
            line += t.delta_line;
            start = t.delta_start;
        } else {
            start += t.delta_start;
        }
        let Some(kind) = kinds.get(t.token_type as usize) else {
            continue;
        };
        let range = index.range_of(
            &lsp_types::Range::new(
                lsp_types::Position::new(line, start),
                lsp_types::Position::new(line, start + t.length),
            ),
            enc,
        );
        if range.is_empty() {
            continue;
        }
        let modifiers = modifier_flags
            .iter()
            .enumerate()
            .filter(|(bit, _)| *bit < 32 && t.token_modifiers_bitset & (1 << bit) != 0)
            .fold(0, |acc, (_, f)| acc | f);
        spans.push(SemanticSpan {
            range,
            kind: Arc::clone(kind),
            modifiers,
        });
    }
    spans
}

/// Whether an inlay hint names a type, a parameter, or something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    Type,
    Parameter,
    Other,
}

/// Extra text the server wants shown at a position, e.g. `: i32`.
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    /// The char the hint is shown in front of.
    pub at: usize,
    pub label: String,
    pub kind: HintKind,
    pub pad_left: bool,
    pub pad_right: bool,
    pub tooltip: Option<String>,
}

pub fn decode_inlay_hints(text: &str, hints: Vec<InlayHint>, enc: Encoding) -> Vec<Hint> {
    let index = LineIndex::new(text);
    let mut out: Vec<Hint> = hints
        .into_iter()
        .filter_map(|h| {
            let label = match h.label {
                InlayHintLabel::String(s) => s,
                InlayHintLabel::LabelParts(parts) => parts.into_iter().map(|p| p.value).collect(),
            };
            // Hints are one line; servers sometimes pad with newlines.
            let label = label.replace('\n', " ");
            if label.trim().is_empty() {
                return None;
            }
            Some(Hint {
                at: index.char_of(h.position, enc),
                label,
                kind: match h.kind {
                    Some(InlayHintKind::TYPE) => HintKind::Type,
                    Some(InlayHintKind::PARAMETER) => HintKind::Parameter,
                    _ => HintKind::Other,
                },
                pad_left: h.padding_left == Some(true),
                pad_right: h.padding_right == Some(true),
                tooltip: h.tooltip.map(|t| match t {
                    InlayHintTooltip::String(s) => s,
                    InlayHintTooltip::MarkupContent(m) => m.value,
                }),
            })
        })
        .collect();
    // Stable, so hints at the same place keep the server's order.
    out.sort_by_key(|h| h.at);
    out
}

/// A symbol in one file, with the symbols nested in it.
#[derive(Debug, Clone, PartialEq)]
pub struct DocSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: SymbolKind,
    /// The whole symbol, e.g. a function including its body.
    pub range: Range<usize>,
    /// The part to select when jumping to it, usually its name.
    pub selection: Range<usize>,
    pub children: Vec<DocSymbol>,
}

/// Symbols as a tree, in file order. Flat lists (older servers) are nested
/// by which symbol's range contains which.
pub fn decode_document_symbols(
    text: &str,
    response: DocumentSymbolResponse,
    enc: Encoding,
) -> Vec<DocSymbol> {
    let index = LineIndex::new(text);
    let mut symbols = match response {
        DocumentSymbolResponse::Nested(symbols) => symbols
            .into_iter()
            .map(|s| nested_symbol(&index, s, enc))
            .collect(),
        DocumentSymbolResponse::Flat(symbols) => nest_flat(&index, symbols, enc),
    };
    sort_symbols(&mut symbols);
    symbols
}

fn nested_symbol(index: &LineIndex, s: DocumentSymbol, enc: Encoding) -> DocSymbol {
    let range = index.range_of(&s.range, enc);
    let selection = index.range_of(&s.selection_range, enc);
    DocSymbol {
        name: s.name,
        detail: s.detail.filter(|d| !d.trim().is_empty()),
        kind: s.kind,
        // Some servers put the name outside the range; keep it inside.
        range: range.start.min(selection.start)..range.end.max(selection.end),
        selection,
        children: s
            .children
            .unwrap_or_default()
            .into_iter()
            .map(|c| nested_symbol(index, c, enc))
            .collect(),
    }
}

#[allow(deprecated)] // `SymbolInformation::deprecated` must be named to build one.
fn nest_flat(index: &LineIndex, symbols: Vec<SymbolInformation>, enc: Encoding) -> Vec<DocSymbol> {
    let mut flat: Vec<DocSymbol> = symbols
        .into_iter()
        .map(|s| {
            let range = index.range_of(&s.location.range, enc);
            DocSymbol {
                name: s.name,
                detail: None,
                kind: s.kind,
                selection: range.clone(),
                range,
                children: Vec::new(),
            }
        })
        .collect();
    // Outer symbols first, so each one is placed before what it contains.
    flat.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(b.range.end.cmp(&a.range.end))
    });
    let mut roots: Vec<DocSymbol> = Vec::new();
    for s in flat {
        insert_nested(&mut roots, s);
    }
    roots
}

fn insert_nested(siblings: &mut Vec<DocSymbol>, s: DocSymbol) {
    if let Some(parent) = siblings.last_mut()
        && parent.range.start <= s.range.start
        && s.range.end <= parent.range.end
        && parent.range != s.range
    {
        insert_nested(&mut parent.children, s);
    } else {
        siblings.push(s);
    }
}

fn sort_symbols(symbols: &mut [DocSymbol]) {
    symbols.sort_by_key(|s| s.range.start);
    for s in symbols {
        sort_symbols(&mut s.children);
    }
}

/// The innermost-last chain of symbols whose range contains `at`.
pub fn symbol_path(symbols: &[DocSymbol], at: usize) -> Vec<&DocSymbol> {
    let mut path = Vec::new();
    let mut level = symbols;
    while let Some(s) = level
        .iter()
        .find(|s| s.range.start <= at && at <= s.range.end)
    {
        path.push(s);
        level = &s.children;
    }
    path
}

/// A symbol somewhere in the workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolLocation {
    pub name: String,
    pub kind: SymbolKind,
    pub container: Option<String>,
    pub location: Location,
}

pub fn decode_workspace_symbols(
    response: WorkspaceSymbolResponse,
    enc: Encoding,
) -> Vec<SymbolLocation> {
    let symbol = |name: String,
                  kind: SymbolKind,
                  container: Option<String>,
                  uri: &lsp_types::Uri,
                  range: lsp_types::Range| {
        Some(SymbolLocation {
            name,
            kind,
            container: container.filter(|c| !c.is_empty()),
            location: Location {
                path: uri_to_path(uri)?,
                range,
                encoding: enc,
            },
        })
    };
    match response {
        WorkspaceSymbolResponse::Flat(symbols) => symbols
            .into_iter()
            .filter_map(|s| {
                symbol(
                    s.name,
                    s.kind,
                    s.container_name,
                    &s.location.uri,
                    s.location.range,
                )
            })
            .collect(),
        WorkspaceSymbolResponse::Nested(symbols) => symbols
            .into_iter()
            .filter_map(|s| {
                let (uri, range) = match s.location {
                    OneOf::Left(l) => (l.uri, l.range),
                    // Only the file is known; its start will do.
                    OneOf::Right(w) => (w.uri, lsp_types::Range::default()),
                };
                symbol(s.name, s.kind, s.container_name, &uri, range)
            })
            .collect(),
    }
}

/// A readable name for a symbol kind.
pub fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::FILE => "file",
        SymbolKind::MODULE => "module",
        SymbolKind::NAMESPACE => "namespace",
        SymbolKind::PACKAGE => "package",
        SymbolKind::CLASS => "class",
        SymbolKind::METHOD => "method",
        SymbolKind::PROPERTY => "property",
        SymbolKind::FIELD => "field",
        SymbolKind::CONSTRUCTOR => "constructor",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::STRING => "string",
        SymbolKind::NUMBER => "number",
        SymbolKind::BOOLEAN => "boolean",
        SymbolKind::ARRAY => "array",
        SymbolKind::OBJECT => "object",
        SymbolKind::KEY => "key",
        SymbolKind::NULL => "null",
        SymbolKind::ENUM_MEMBER => "variant",
        SymbolKind::STRUCT => "struct",
        SymbolKind::EVENT => "event",
        SymbolKind::OPERATOR => "operator",
        SymbolKind::TYPE_PARAMETER => "type parameter",
        _ => "symbol",
    }
}

/// Byte range of a parameter within its signature's label, for highlighting
/// it. Offsets count in the server's position encoding.
pub fn parameter_range(
    signature: &str,
    label: &ParameterLabel,
    enc: Encoding,
) -> Option<Range<usize>> {
    match label {
        ParameterLabel::Simple(name) => {
            if name.is_empty() {
                return None;
            }
            // Match a whole word, so `a` doesn't light up the `a` in `fn add`.
            let bytes = signature.as_bytes();
            let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
            signature.match_indices(name.as_str()).find_map(|(i, _)| {
                let end = i + name.len();
                let before = i == 0 || !is_word(bytes[i - 1]);
                let after = end == bytes.len() || !is_word(bytes[end]);
                (before && after).then_some(i..end)
            })
        }
        ParameterLabel::LabelOffsets([start, end]) => {
            let index = LineIndex::new(signature);
            let to_byte = |units: u32| {
                let ch = index.char_of(lsp_types::Position::new(0, units), enc);
                signature
                    .char_indices()
                    .nth(ch)
                    .map_or(signature.len(), |(b, _)| b)
            };
            let r = to_byte(*start)..to_byte(*end);
            (r.start < r.end).then_some(r)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{SemanticTokenModifier, SemanticTokenType};
    use serde_json::json;

    #[test]
    fn decodes_relative_semantic_tokens() {
        let text = "fn add(a: i32) {}\n  añb(x)\n";
        let legend = SemanticTokensLegend {
            token_types: vec![SemanticTokenType::FUNCTION, SemanticTokenType::PARAMETER],
            token_modifiers: vec![
                SemanticTokenModifier::DECLARATION,
                SemanticTokenModifier::READONLY,
            ],
        };
        let tok = |dl, ds, len, ty, m| lsp_types::SemanticToken {
            delta_line: dl,
            delta_start: ds,
            length: len,
            token_type: ty,
            token_modifiers_bitset: m,
        };
        // `add`, `a`, then `añb` and `x` on the next line (UTF-16 columns),
        // and one with a type outside the legend.
        let data = [
            tok(0, 3, 3, 0, 1),
            tok(0, 4, 1, 1, 2),
            tok(1, 2, 3, 0, 0),
            tok(0, 4, 1, 1, 0),
            tok(0, 1, 1, 9, 0),
        ];
        let spans = decode_semantic_tokens(text, &data, &legend, Encoding::Utf16);
        let got: Vec<_> = spans
            .iter()
            .map(|s| (s.range.clone(), &*s.kind, s.modifiers))
            .collect();
        assert_eq!(
            got,
            [
                (3..6, "function", 0),
                (7..8, "parameter", token_mods::READONLY),
                (20..23, "function", 0),
                (24..25, "parameter", 0),
            ]
        );
    }

    #[test]
    fn decodes_inlay_hints_in_order() {
        let hints: Vec<InlayHint> = serde_json::from_value(json!([
            { "position": { "line": 1, "character": 5 }, "label": "x:", "kind": 2, "paddingRight": true },
            { "position": { "line": 0, "character": 5 }, "label": [{ "value": ": " }, { "value": "Vec<i32>" }], "kind": 1 },
            { "position": { "line": 0, "character": 6 }, "label": "  " },
        ]))
        .unwrap();
        let text = "let v = vec![];\nfoo(1);\n";
        let got = decode_inlay_hints(text, hints, Encoding::Utf16);
        assert_eq!(got.len(), 2, "blank hints are dropped");
        assert_eq!((got[0].at, got[0].label.as_str()), (5, ": Vec<i32>"));
        assert_eq!(got[0].kind, HintKind::Type);
        assert_eq!(
            (got[1].at, got[1].kind, got[1].pad_right),
            (21, HintKind::Parameter, true)
        );
    }

    #[test]
    fn nests_flat_symbols_by_range() {
        let uri = "file:///tmp/a.rs";
        let sym = |name: &str, kind: u32, l0: u32, l1: u32| {
            json!({ "name": name, "kind": kind, "location": { "uri": uri,
                "range": { "start": { "line": l0, "character": 0 }, "end": { "line": l1, "character": 1 } } } })
        };
        let response: DocumentSymbolResponse = serde_json::from_value(json!([
            sym("method", 6, 2, 3),
            sym("Thing", 5, 1, 4),
            sym("free", 12, 6, 6),
        ]))
        .unwrap();
        let text = "\nclass Thing {\n  method() {\n  }\n}\n\nfree\n";
        let symbols = decode_document_symbols(text, response, Encoding::Utf16);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "Thing");
        assert_eq!(symbols[0].children[0].name, "method");
        assert_eq!(symbols[1].name, "free");

        let inside_method = text.find("() {").unwrap();
        let path: Vec<_> = symbol_path(&symbols, inside_method)
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(path, ["Thing", "method"]);
        assert!(symbol_path(&symbols, 0).is_empty());
    }

    #[test]
    fn nested_symbols_keep_their_name_inside_the_range() {
        let response: DocumentSymbolResponse = serde_json::from_value(json!([{
            "name": "f", "kind": 12,
            "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 9 } },
            "selectionRange": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "children": [],
        }]))
        .unwrap();
        let symbols = decode_document_symbols("f() { 1 }", response, Encoding::Utf16);
        assert_eq!(symbols[0].range, 0..9);
        assert_eq!(symbols[0].selection, 0..1);
    }

    #[test]
    fn decodes_workspace_symbols() {
        // A real path, so the URI converts back on every platform (a bare
        // `file:///tmp/…` has no drive letter on Windows).
        let file = std::env::temp_dir().join("foo.rs");
        let uri = crate::lsp::text::path_to_uri(&file).unwrap();
        let response: WorkspaceSymbolResponse = serde_json::from_value(json!([
            { "name": "Foo", "kind": 23, "containerName": "",
              "location": { "uri": uri } },
            { "name": "bar", "kind": 12, "containerName": "Foo",
              "location": { "uri": uri,
                "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 7 } } } },
        ]))
        .unwrap();
        let symbols = decode_workspace_symbols(response, Encoding::Utf8);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].container, None);
        assert_eq!(symbols[1].container.as_deref(), Some("Foo"));
        assert_eq!(symbols[1].location.range.start.line, 3);
        assert_eq!(symbols[1].location.encoding, Encoding::Utf8);
        assert_eq!(symbols[1].location.path, file);
    }

    #[test]
    fn finds_parameter_in_signature() {
        let sig = "fn add(a: i32, b: i32) -> i32";
        let simple = |s: &str| ParameterLabel::Simple(s.to_owned());
        assert_eq!(
            parameter_range(sig, &simple("a: i32"), Encoding::Utf16),
            Some(7..13)
        );
        // `a` alone must not match inside `add`.
        assert_eq!(
            parameter_range(sig, &simple("a"), Encoding::Utf16),
            Some(7..8)
        );
        assert_eq!(
            parameter_range(
                sig,
                &ParameterLabel::LabelOffsets([15, 21]),
                Encoding::Utf16
            ),
            Some(15..21)
        );
        // Offsets count UTF-16 units; `é` is one unit but two bytes.
        let sig = "f(é, x)";
        assert_eq!(
            parameter_range(sig, &ParameterLabel::LabelOffsets([5, 6]), Encoding::Utf16),
            Some(6..7)
        );
        assert_eq!(parameter_range(sig, &simple("zz"), Encoding::Utf16), None);
    }
}

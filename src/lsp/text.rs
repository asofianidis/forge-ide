//! Converting between the editor's view of text (character indices, file
//! paths) and the protocol's (line/character positions in a negotiated
//! encoding, `file://` URIs), plus snippet expansion and applying text edits.

use std::ops::Range;
use std::path::{Path, PathBuf};

use lsp_types::{Position, Uri};

/// What a position's `character` counts. UTF-16 is the protocol default;
/// servers may pick UTF-8 or UTF-32 when the client offers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    Utf8,
    #[default]
    Utf16,
    Utf32,
}

impl Encoding {
    pub fn from_kind(kind: Option<&lsp_types::PositionEncodingKind>) -> Self {
        match kind.map(|k| k.as_str()) {
            Some("utf-8") => Encoding::Utf8,
            Some("utf-32") => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    fn units(self, c: char) -> usize {
        match self {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
            Encoding::Utf32 => 1,
        }
    }
}

/// Where each line starts, so positions convert in time proportional to the
/// line's length rather than the file's.
pub struct LineIndex<'a> {
    text: &'a str,
    /// (byte offset, char offset) of each line start.
    starts: Vec<(usize, usize)>,
    total_chars: usize,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut starts = vec![(0, 0)];
        let mut chars = 0;
        for (b, c) in text.char_indices() {
            chars += 1;
            if c == '\n' {
                starts.push((b + 1, chars));
            }
        }
        Self {
            text,
            starts,
            total_chars: chars,
        }
    }

    /// Text of line `line`, without its `\n`.
    fn line(&self, line: usize) -> &'a str {
        let start = self.starts[line].0;
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.text.len(), |&(b, _)| b - 1);
        &self.text[start..end]
    }

    /// Character index of `pos`, clamped to the end of its line (and of the text).
    pub fn char_of(&self, pos: Position, enc: Encoding) -> usize {
        let line = pos.line as usize;
        let Some(&(_, line_start)) = self.starts.get(line) else {
            return self.total_chars;
        };
        let target = pos.character as usize;
        let mut units = 0;
        let mut chars = 0;
        for c in self.line(line).chars() {
            if units >= target {
                break;
            }
            units += enc.units(c);
            chars += 1;
        }
        line_start + chars
    }

    pub fn position_of(&self, char_idx: usize, enc: Encoding) -> Position {
        let char_idx = char_idx.min(self.total_chars);
        let line = self.starts.partition_point(|&(_, c)| c <= char_idx) - 1;
        let column = char_idx - self.starts[line].1;
        let character = self
            .line(line)
            .chars()
            .take(column)
            .map(|c| enc.units(c))
            .sum::<usize>();
        Position::new(line as u32, character as u32)
    }

    pub fn range_of(&self, range: &lsp_types::Range, enc: Encoding) -> Range<usize> {
        let start = self.char_of(range.start, enc);
        let end = self.char_of(range.end, enc).max(start);
        start..end
    }
}

pub fn path_to_uri(path: &Path) -> Option<Uri> {
    url::Url::from_file_path(path).ok()?.as_str().parse().ok()
}

pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

/// The protocol's language identifier for a file.
pub fn language_id(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        "Dockerfile" | "Containerfile" => return "dockerfile".into(),
        "go.mod" => return "go.mod".into(),
        "go.work" => return "go.work".into(),
        "Gemfile" | "Rakefile" => return "ruby".into(),
        "Makefile" | "makefile" => return "makefile".into(),
        _ => {}
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let id = match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "m" => "objective-c",
        "mm" => "objective-cpp",
        "cs" | "csx" => "csharp",
        "htm" | "html" => "html",
        "md" | "markdown" => "markdown",
        "ml" => "ocaml",
        "mli" => "ocaml.interface",
        "rb" | "rake" | "gemspec" => "ruby",
        "sh" | "bash" => "shellscript",
        "tf" => "terraform",
        "tfvars" => "terraform-vars",
        "yml" | "yaml" => "yaml",
        "hs" => "haskell",
        "lhs" => "lhaskell",
        "zon" => "zig",
        "kt" | "kts" => "kotlin",
        other => other,
    };
    id.to_owned()
}

/// Characters that make up identifiers, for finding the word being completed.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(b, _)| b)
}

/// Start of the identifier that ends at `char_idx`.
pub fn word_start(text: &str, char_idx: usize) -> usize {
    let b = char_to_byte(text, char_idx);
    let len = text[..b]
        .chars()
        .rev()
        .take_while(|&c| is_word_char(c))
        .count();
    char_idx - len
}

/// The identifier containing (or touching) `char_idx`, if any.
pub fn word_at(text: &str, char_idx: usize) -> Option<Range<usize>> {
    let b = char_to_byte(text, char_idx);
    let after = text[b..].chars().take_while(|&c| is_word_char(c)).count();
    let start = word_start(text, char_idx);
    (start < char_idx + after).then_some(start..char_idx + after)
}

/// A snippet with its placeholders filled in by their defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// Chars to select (or the caret) for the first tab stop, relative to the
    /// start of `text`; `None` puts the caret at the end.
    pub selection: Option<Range<usize>>,
}

/// Expands LSP snippet syntax (`$1`, `${1:name}`, `${1|a,b|}`, `$0`,
/// variables) into plain text. Only the first tab stop is kept, as the caret
/// or selection after inserting.
pub fn expand_snippet(src: &str) -> Snippet {
    let chars: Vec<char> = src.chars().collect();
    let mut p = SnippetParser {
        chars: &chars,
        i: 0,
        out: String::new(),
        len: 0,
        stops: Vec::new(),
    };
    p.body(false);
    let first = p
        .stops
        .iter()
        .filter(|(n, _)| *n > 0)
        .min_by_key(|(n, _)| *n)
        .or_else(|| p.stops.iter().find(|(n, _)| *n == 0))
        .map(|(_, r)| r.clone());
    Snippet {
        text: p.out,
        selection: first,
    }
}

struct SnippetParser<'a> {
    chars: &'a [char],
    i: usize,
    out: String,
    /// Chars in `out`.
    len: usize,
    stops: Vec<(u32, Range<usize>)>,
}

impl SnippetParser<'_> {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.i + ahead).copied()
    }

    fn push(&mut self, c: char) {
        self.out.push(c);
        self.len += 1;
    }

    /// Parses until the end, or until an unmatched `}` when `nested`.
    fn body(&mut self, nested: bool) {
        while let Some(c) = self.peek(0) {
            match c {
                '\\' if matches!(self.peek(1), Some('$' | '}' | '\\')) => {
                    self.push(self.chars[self.i + 1]);
                    self.i += 2;
                }
                '}' if nested => return,
                '$' if self.dollar() => {}
                _ => {
                    self.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        let digits: String = self.chars[start..self.i].iter().collect();
        digits.parse().ok()
    }

    fn name(&mut self) -> bool {
        let start = self.i;
        while self
            .peek(0)
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.i += 1;
        }
        self.i > start
    }

    fn add_stop(&mut self, n: u32, start: usize) {
        if !self.stops.iter().any(|(m, _)| *m == n) {
            self.stops.push((n, start..self.len));
        }
    }

    /// Parses a `$…` construct at the cursor. Returns false (consuming
    /// nothing) if it isn't one, so the `$` is literal.
    fn dollar(&mut self) -> bool {
        let save = self.i;
        let start = self.len;
        self.i += 1;
        match self.peek(0) {
            Some(c) if c.is_ascii_digit() => {
                let n = self.number().unwrap_or(0);
                self.add_stop(n, start);
                true
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                // A variable such as $TM_FILENAME: we have no values for them.
                self.name();
                true
            }
            Some('{') => {
                self.i += 1;
                let stop = if self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    self.number()
                } else if self.name() {
                    None
                } else {
                    self.i = save;
                    return false;
                };
                match self.peek(0) {
                    Some('}') => self.i += 1,
                    Some(':') => {
                        self.i += 1;
                        self.body(true);
                        if self.peek(0) == Some('}') {
                            self.i += 1;
                        }
                    }
                    Some('|') if stop.is_some() => {
                        self.i += 1;
                        let mut first = true;
                        while let Some(c) = self.peek(0) {
                            self.i += 1;
                            match c {
                                '|' if self.peek(0) == Some('}') => {
                                    self.i += 1;
                                    break;
                                }
                                ',' => first = false,
                                '\\' => {
                                    if let Some(next) = self.peek(0) {
                                        self.i += 1;
                                        if first {
                                            self.push(next);
                                        }
                                    }
                                }
                                _ if first => self.push(c),
                                _ => {}
                            }
                        }
                    }
                    _ => {
                        self.i = save;
                        self.len = start;
                        return false;
                    }
                }
                if let Some(n) = stop {
                    self.add_stop(n, start);
                }
                true
            }
            _ => {
                self.i = save;
                false
            }
        }
    }
}

/// A replacement of a range of characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharEdit {
    pub range: Range<usize>,
    pub text: String,
}

/// Converts protocol edits against `text` into character edits.
pub fn char_edits(text: &str, edits: &[lsp_types::TextEdit], enc: Encoding) -> Vec<CharEdit> {
    let index = LineIndex::new(text);
    edits
        .iter()
        .map(|e| CharEdit {
            range: index.range_of(&e.range, enc),
            text: e.new_text.clone(),
        })
        .collect()
}

/// Applies non-overlapping edits (in any order) to `text`. Edits that
/// overlap an earlier one are skipped.
pub fn apply_edits(text: &str, edits: &[CharEdit]) -> String {
    let mut sorted: Vec<&CharEdit> = edits.iter().collect();
    // Stable, so several inserts at one spot keep the server's order.
    sorted.sort_by_key(|e| e.range.start);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    let (mut char_pos, mut byte_pos) = (0, 0);
    let mut advance = |target: usize| {
        while char_pos < target {
            let Some(c) = chars.next() else { break };
            char_pos += 1;
            byte_pos += c.len_utf8();
        }
        (char_pos, byte_pos)
    };
    let (mut copied_char, mut copied_byte) = (0, 0);
    for edit in sorted {
        if edit.range.start < copied_char {
            continue;
        }
        let (_, start) = advance(edit.range.start);
        out.push_str(&text[copied_byte..start]);
        out.push_str(&edit.text);
        (copied_char, copied_byte) = advance(edit.range.end.max(edit.range.start));
    }
    out.push_str(&text[copied_byte..]);
    out
}

/// Where character `offset` ends up after `edits` are applied. An offset
/// inside a replaced range moves to the end of the replacement; text inserted
/// exactly at the offset goes before it.
pub fn map_offset(offset: usize, edits: &[CharEdit]) -> usize {
    let growth = |e: &CharEdit| e.text.chars().count() as isize - e.range.len() as isize;
    let shift_before =
        |at: usize| -> isize { edits.iter().filter(|e| e.range.end <= at).map(growth).sum() };
    if let Some(e) = edits
        .iter()
        .find(|e| e.range.start < offset && offset < e.range.end)
    {
        let start = e.range.start as isize + shift_before(e.range.start);
        return (start + e.text.chars().count() as isize).max(0) as usize;
    }
    (offset as isize + shift_before(offset)).max(0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_in_every_encoding() {
        let text = "a😀b\nñx\r\n\nend";
        let index = LineIndex::new(text);
        for enc in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for i in 0..=text.chars().count() {
                let pos = index.position_of(i, enc);
                assert_eq!(index.char_of(pos, enc), i, "{enc:?} char {i} -> {pos:?}");
            }
        }
        // The emoji is 4 UTF-8 bytes, 2 UTF-16 units, 1 char.
        assert_eq!(index.position_of(2, Encoding::Utf8), Position::new(0, 5));
        assert_eq!(index.position_of(2, Encoding::Utf16), Position::new(0, 3));
        assert_eq!(index.position_of(2, Encoding::Utf32), Position::new(0, 2));
        assert_eq!(index.position_of(5, Encoding::Utf16), Position::new(1, 1));
    }

    #[test]
    fn out_of_range_positions_clamp() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(index.char_of(Position::new(0, 99), Encoding::Utf16), 2);
        assert_eq!(index.char_of(Position::new(9, 0), Encoding::Utf16), 5);
        assert_eq!(index.position_of(99, Encoding::Utf16), Position::new(1, 2));
    }

    #[test]
    fn uris_round_trip() {
        let path = std::env::temp_dir().join("forge dir").join("a#b.rs");
        let uri = path_to_uri(&path).unwrap();
        assert!(uri.as_str().starts_with("file://"));
        assert!(uri.as_str().contains("forge%20dir"));
        assert_eq!(uri_to_path(&uri), Some(path));
    }

    #[test]
    fn words() {
        let text = "let foo_bar = x.baz;";
        assert_eq!(word_start(text, 11), 4);
        assert_eq!(word_start(text, 4), 4);
        assert_eq!(word_at(text, 6), Some(4..11));
        assert_eq!(word_at(text, 11), Some(4..11));
        assert_eq!(word_at(text, 12), None);
    }

    #[test]
    fn snippets() {
        let s = expand_snippet("println!(\"$1\")$0");
        assert_eq!(s.text, "println!(\"\")");
        assert_eq!(s.selection, Some(10..10));

        let s = expand_snippet("fn ${1:name}(${2:args}) {\n\t$0\n}");
        assert_eq!(s.text, "fn name(args) {\n\t\n}");
        assert_eq!(s.selection, Some(3..7));

        let s = expand_snippet("${1|one,two|} \\$x ${TM_FILENAME} ${VAR:def} $");
        assert_eq!(s.text, "one $x  def $");
        assert_eq!(s.selection, Some(0..3));

        let s = expand_snippet("${1:outer ${2:inner}}");
        assert_eq!(s.text, "outer inner");
        assert_eq!(s.selection, Some(0..11));

        assert_eq!(expand_snippet("plain").selection, None);
    }

    #[test]
    fn applies_edits_in_any_order() {
        let text = "héllo world";
        let edits = vec![
            CharEdit {
                range: 6..11,
                text: "there".into(),
            },
            CharEdit {
                range: 0..1,
                text: "J".into(),
            },
            CharEdit {
                range: 5..5,
                text: ",".into(),
            },
        ];
        assert_eq!(apply_edits(text, &edits), "Jéllo, there");
    }

    #[test]
    fn maps_offsets_through_edits() {
        // "abc def" -> "XXabc d" (insert at 0, delete "ef").
        let edits = vec![
            CharEdit {
                range: 0..0,
                text: "XX".into(),
            },
            CharEdit {
                range: 5..7,
                text: String::new(),
            },
        ];
        assert_eq!(map_offset(3, &edits), 5);
        assert_eq!(map_offset(0, &edits), 2);
        assert_eq!(map_offset(7, &edits), 7);
        // Inside the replaced range: to the end of the replacement.
        assert_eq!(map_offset(6, &edits), 7);
    }

    #[test]
    fn converts_lsp_edits() {
        let text = "a😀b\nc";
        let edit = lsp_types::TextEdit::new(
            lsp_types::Range::new(Position::new(0, 3), Position::new(1, 0)),
            "-".into(),
        );
        let edits = char_edits(text, &[edit], Encoding::Utf16);
        assert_eq!(edits[0].range, 2..4);
        assert_eq!(apply_edits(text, &edits), "a😀-c");
    }
}

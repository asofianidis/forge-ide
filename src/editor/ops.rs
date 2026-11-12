//! Editing operations: comment toggling, indentation, duplicating and moving
//! lines, auto-indent and auto-closing pairs while typing, and bracket matching.
//!
//! Every operation works on plain text and a selection given as character
//! indices (what egui's `CCursor` uses), and returns the new text together with
//! the selection to restore, so they can be unit-tested without a UI.

use std::ops::Range;

/// A selection as character indices. `start <= end`; equal means a caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

impl Selection {
    pub fn new(a: usize, b: usize) -> Self {
        Self {
            start: a.min(b),
            end: a.max(b),
        }
    }

    pub fn caret(at: usize) -> Self {
        Self { start: at, end: at }
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// The result of an editing operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub selection: Selection,
}

/// How a language writes comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStyle {
    Line(&'static str),
    Block(&'static str, &'static str),
}

/// Comment syntax for a file extension, or `None` if unknown.
pub fn comment_style(extension: &str) -> Option<CommentStyle> {
    use CommentStyle::*;
    let style = match extension.to_ascii_lowercase().as_str() {
        "rs" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "cs" | "java" | "kt" | "kts"
        | "scala" | "go" | "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx"
        | "swift" | "dart" | "zig" | "php" | "groovy" | "gradle" | "proto" | "jsonc" | "scss"
        | "less" | "sol" | "v" | "glsl" | "wgsl" | "hlsl" | "m" | "mm" | "fs" | "fsx" => Line("//"),
        "py" | "pyi" | "rb" | "sh" | "bash" | "zsh" | "fish" | "pl" | "pm" | "r" | "toml"
        | "yaml" | "yml" | "ex" | "exs" | "nix" | "cmake" | "mk" | "dockerfile" | "conf"
        | "ini" | "cfg" | "ps1" | "jl" | "tf" | "hcl" | "gitignore" | "env" | "nim" | "coffee"
        | "cr" | "makefile" => Line("#"),
        "lua" | "sql" | "hs" | "elm" | "ada" | "adb" | "ads" => Line("--"),
        "clj" | "cljs" | "edn" | "lisp" | "el" | "scm" | "rkt" | "asm" | "s" => Line(";"),
        "tex" | "sty" | "erl" | "hrl" | "matlab" => Line("%"),
        "vim" => Line("\""),
        "bat" | "cmd" => Line("REM"),
        "css" => Block("/*", "*/"),
        "html" | "htm" | "xml" | "svg" | "vue" | "svelte" | "md" | "markdown" | "xaml" => {
            Block("<!--", "-->")
        }
        "ml" | "mli" => Block("(*", "*)"),
        _ => return None,
    };
    Some(style)
}

/// The indentation to insert for this text: a tab if most indented lines use
/// tabs, otherwise the smallest space indent in use (2–8, default 4).
pub fn indent_unit(text: &str) -> String {
    let mut tabs = 0;
    let mut spaces = 0;
    let mut smallest = usize::MAX;
    for line in text.lines().take(2_000) {
        if line.starts_with('\t') {
            tabs += 1;
        } else if line.starts_with(' ') {
            let n = line.len() - line.trim_start_matches(' ').len();
            if line.trim().is_empty() {
                continue;
            }
            spaces += 1;
            smallest = smallest.min(n);
        }
    }
    if tabs > spaces {
        "\t".to_owned()
    } else if spaces == 0 {
        "    ".to_owned()
    } else {
        " ".repeat(smallest.clamp(2, 8))
    }
}

// ---- offsets --------------------------------------------------------------

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(b, _)| b)
}

pub fn byte_to_char(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().count()
}

/// Character index of the start of 1-based `line`, clamped to the text.
pub fn line_start(text: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut chars = 0;
    let mut current = 1;
    for c in text.chars() {
        chars += 1;
        if c == '\n' {
            current += 1;
            if current == line {
                return chars;
            }
        }
    }
    // Past the end: the start of the last line.
    text.rfind('\n').map_or(0, |b| byte_to_char(text, b + 1))
}

/// Character index of 1-based (`line`, `column`), clamped to that line.
#[cfg(test)]
pub fn line_col_to_char(text: &str, line: usize, column: usize) -> usize {
    let start = line_start(text, line);
    let line_len = text.chars().skip(start).take_while(|&c| c != '\n').count();
    start + column.saturating_sub(1).min(line_len)
}

/// Byte range of the full lines touched by `sel` (without the final newline).
///
/// A selection that ends at the very start of a line doesn't include that line,
/// matching how other editors treat "select whole lines with the mouse".
fn line_block(text: &str, sel: Selection) -> Range<usize> {
    let start = char_to_byte(text, sel.start);
    let mut end = char_to_byte(text, sel.end);
    if end > start && text[..end].ends_with('\n') {
        end -= 1;
    }
    let block_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let block_end = text[end..].find('\n').map_or(text.len(), |i| end + i);
    block_start..block_end.max(block_start)
}

/// Start byte of each line in `block` (a range of whole lines).
fn line_starts(text: &str, block: Range<usize>) -> Vec<usize> {
    let mut starts = vec![block.start];
    starts.extend(
        text[block.clone()]
            .match_indices('\n')
            .map(|(i, _)| block.start + i + 1),
    );
    starts
}

fn line_at(text: &str, start: usize) -> &str {
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    &text[start..end]
}

/// A replacement of `range` (bytes in the old text) with `with`.
struct Change {
    range: Range<usize>,
    with: String,
}

/// Applies non-overlapping `changes` (sorted by position) and maps the
/// selection through them.
fn apply(text: &str, changes: Vec<Change>, sel: Selection) -> Edit {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for change in &changes {
        out.push_str(&text[last..change.range.start]);
        out.push_str(&change.with);
        last = change.range.end;
    }
    out.push_str(&text[last..]);

    // A caret sitting exactly where text is inserted moves past the insertion;
    // the start of a real selection stays put so the inserted text is selected.
    let map = |pos: usize, sticky_left: bool| -> usize {
        let mut delta = 0isize;
        for c in &changes {
            if pos < c.range.start {
                break;
            }
            if pos == c.range.start && (!c.range.is_empty() || sticky_left) {
                break;
            }
            if pos < c.range.end {
                let offset = (pos - c.range.start).min(c.with.len());
                return (c.range.start as isize + delta) as usize + offset;
            }
            delta += c.with.len() as isize - c.range.len() as isize;
        }
        (pos as isize + delta) as usize
    };
    let start = map(char_to_byte(text, sel.start), !sel.is_empty());
    let end = map(char_to_byte(text, sel.end), false);
    let start = floor_char_boundary(&out, start);
    let end = floor_char_boundary(&out, end);
    Edit {
        selection: Selection::new(byte_to_char(&out, start), byte_to_char(&out, end)),
        text: out,
    }
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn leading_whitespace(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

// ---- operations -----------------------------------------------------------

/// Replaces the selection with `insert`, leaving a caret after it.
#[cfg(test)]
pub fn insert(text: &str, sel: Selection, insert: &str) -> Edit {
    let range = char_to_byte(text, sel.start)..char_to_byte(text, sel.end);
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..range.start]);
    out.push_str(insert);
    out.push_str(&text[range.end..]);
    Edit {
        text: out,
        selection: Selection::caret(sel.start + insert.chars().count()),
    }
}

/// Comments out the selected lines, or uncomments them if they all are.
pub fn toggle_comment(text: &str, sel: Selection, style: CommentStyle) -> Edit {
    let block = line_block(text, sel);
    let starts = line_starts(text, block);
    let lines: Vec<(usize, &str)> = starts.iter().map(|&s| (s, line_at(text, s))).collect();
    let non_blank: Vec<&(usize, &str)> =
        lines.iter().filter(|(_, l)| !l.trim().is_empty()).collect();
    // Comment blank lines only when nothing else is selected.
    let targets: Vec<&(usize, &str)> = if non_blank.is_empty() {
        lines.iter().collect()
    } else {
        non_blank
    };

    let is_commented = |line: &str| {
        let t = line.trim();
        match style {
            CommentStyle::Line(p) => t.starts_with(p),
            CommentStyle::Block(open, close) => {
                t.starts_with(open) && t.ends_with(close) && t.len() >= open.len() + close.len()
            }
        }
    };
    let all_commented = targets.iter().all(|(_, l)| is_commented(l));

    let mut changes = Vec::new();
    if all_commented && !targets.iter().all(|(_, l)| l.trim().is_empty()) {
        for &&(start, line) in &targets {
            let indent = leading_whitespace(line);
            let at = start + indent;
            let (open, close) = match style {
                CommentStyle::Line(p) => (p, None),
                CommentStyle::Block(o, c) => (o, Some(c)),
            };
            let mut open_len = open.len();
            if line[indent + open_len..].starts_with(' ') {
                open_len += 1;
            }
            changes.push(Change {
                range: at..at + open_len,
                with: String::new(),
            });
            if let Some(close) = close {
                let trimmed_end = start + line.trim_end().len();
                let mut close_start = trimmed_end - close.len();
                if close_start > at + open_len && text[..close_start].ends_with(' ') {
                    close_start -= 1;
                }
                changes.push(Change {
                    range: close_start..trimmed_end,
                    with: String::new(),
                });
            }
        }
    } else {
        let indent = targets
            .iter()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(_, l)| leading_whitespace(l))
            .min()
            .unwrap_or(0);
        for &&(start, line) in &targets {
            let at = start + indent.min(line.len());
            match style {
                CommentStyle::Line(p) => changes.push(Change {
                    range: at..at,
                    with: format!("{p} "),
                }),
                CommentStyle::Block(open, close) => {
                    changes.push(Change {
                        range: at..at,
                        with: format!("{open} "),
                    });
                    let end = start + line.trim_end().len().max(indent.min(line.len()));
                    changes.push(Change {
                        range: end..end,
                        with: format!(" {close}"),
                    });
                }
            }
        }
    }
    apply(text, changes, sel)
}

/// Indents every selected line by `unit`. Blank lines are left alone unless
/// the selection covers just one line.
pub fn indent(text: &str, sel: Selection, unit: &str) -> Edit {
    let block = line_block(text, sel);
    let starts = line_starts(text, block);
    let single = starts.len() == 1;
    let changes = starts
        .into_iter()
        .filter(|&s| single || !line_at(text, s).trim().is_empty())
        .map(|s| Change {
            range: s..s,
            with: unit.to_owned(),
        })
        .collect();
    apply(text, changes, sel)
}

/// Removes one level of indentation (a tab, or up to `unit`'s width in spaces)
/// from every selected line.
pub fn outdent(text: &str, sel: Selection, unit: &str) -> Edit {
    let width = if unit == "\t" { 4 } else { unit.len().max(1) };
    let block = line_block(text, sel);
    let changes = line_starts(text, block)
        .into_iter()
        .filter_map(|s| {
            let line = line_at(text, s);
            let n = if line.starts_with('\t') {
                1
            } else {
                line.len() - line.trim_start_matches(' ').len()
            }
            .min(width);
            (n > 0).then(|| Change {
                range: s..s + n,
                with: String::new(),
            })
        })
        .collect();
    apply(text, changes, sel)
}

/// Copies the selected lines below themselves and selects the copy.
pub fn duplicate_lines(text: &str, sel: Selection) -> Edit {
    let block = line_block(text, sel);
    let copied = &text[block.clone()];
    let mut out = String::with_capacity(text.len() + copied.len() + 1);
    out.push_str(&text[..block.end]);
    out.push('\n');
    out.push_str(copied);
    out.push_str(&text[block.end..]);
    let shift = copied.chars().count() + 1;
    Edit {
        text: out,
        selection: Selection {
            start: sel.start + shift,
            end: sel.end + shift,
        },
    }
}

/// Swaps the selected lines with the line above (`up`) or below. Returns
/// `None` at the start/end of the text.
pub fn move_lines(text: &str, sel: Selection, up: bool) -> Option<Edit> {
    let block = line_block(text, sel);
    let (before, moved, shift): (Range<usize>, Range<usize>, isize) = if up {
        if block.start == 0 {
            return None;
        }
        let prev_start = text[..block.start - 1].rfind('\n').map_or(0, |i| i + 1);
        let prev = prev_start..block.start - 1;
        let shift = -(text[prev.clone()].chars().count() as isize + 1);
        (prev, block, shift)
    } else {
        if block.end >= text.len() {
            return None;
        }
        let next_start = block.end + 1;
        let next_end = text[next_start..]
            .find('\n')
            .map_or(text.len(), |i| next_start + i);
        let shift = text[next_start..next_end].chars().count() as isize + 1;
        (block, next_start..next_end, shift)
    };
    // `before` is the upper line(s), `moved` the lower; swap them.
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..before.start]);
    out.push_str(&text[moved.clone()]);
    out.push('\n');
    out.push_str(&text[before.clone()]);
    out.push_str(&text[moved.end..]);
    let move_by = |c: usize| (c as isize + shift).max(0) as usize;
    Some(Edit {
        text: out,
        selection: Selection {
            start: move_by(sel.start),
            end: move_by(sel.end),
        },
    })
}

// ---- typing -----------------------------------------------------------------

/// Quote characters that auto-close in this language. Languages where `'` is
/// mostly an apostrophe or a lifetime (prose, Rust, Lisps) don't pair it.
pub fn auto_close_quotes(extension: &str) -> &'static [char] {
    match extension.to_ascii_lowercase().as_str() {
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "go" | "sh" | "bash"
        | "zsh" | "sql" => &['"', '\'', '`'],
        "md" | "markdown" => &['"', '`'],
        "rs" | "ml" | "mli" | "clj" | "cljs" | "edn" | "lisp" | "el" | "scm" | "rkt" | "tex"
        | "sty" | "vim" => &['"'],
        "txt" | "text" | "log" => &[],
        _ => &['"', '\''],
    }
}

fn closer_for(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

fn is_closer(c: char) -> bool {
    matches!(c, ')' | ']' | '}')
}

/// Languages where a line ending in `:` opens an indented block.
fn colon_opens_block(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "py" | "pyi" | "nim" | "yaml" | "yml"
    )
}

/// Handles typing `ch` over `sel` when it should do more than insert itself:
/// wrap a selection in brackets or quotes, auto-close an opening bracket or
/// quote, or step over the closing character that is already there.
///
/// Returns `None` when plain insertion is right.
pub fn type_char(text: &str, sel: Selection, ch: char, quotes: &[char]) -> Option<Edit> {
    let start = char_to_byte(text, sel.start);
    let end = char_to_byte(text, sel.end);
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let is_quote = quotes.contains(&ch);
    let close = closer_for(ch).or(is_quote.then_some(ch));

    if !sel.is_empty() {
        // Wrap the selection, keeping it selected.
        let close = close?;
        let mut out = String::with_capacity(text.len() + 2);
        out.push_str(&text[..start]);
        out.push(ch);
        out.push_str(&text[start..end]);
        out.push(close);
        out.push_str(&text[end..]);
        return Some(Edit {
            text: out,
            selection: Selection::new(sel.start + 1, sel.end + 1),
        });
    }

    // Step over a closer that's already there instead of doubling it, as long
    // as it actually closes something (so a stray one can still be typed).
    let steps_over = after == Some(ch)
        && ((is_closer(ch) && scan_backward(text.as_bytes(), end).is_some()) || is_quote);
    if steps_over {
        return Some(Edit {
            text: text.to_owned(),
            selection: Selection::caret(sel.start + 1),
        });
    }

    let close = close?;
    // Only auto-close in front of whitespace, a closer or punctuation, so that
    // typing `(` right before a word doesn't produce `()word`.
    let room_after = after.is_none_or(|c| c.is_whitespace() || is_closer(c) || ";,.:".contains(c));
    if !room_after {
        return None;
    }
    // A quote right after a word character is an apostrophe or a string
    // prefix (`it's`, `r"`), and after the same quote it's ending a string.
    if is_quote && before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ch) {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push_str(&text[..start]);
    out.push(ch);
    out.push(close);
    out.push_str(&text[end..]);
    Some(Edit {
        text: out,
        selection: Selection::caret(sel.start + 1),
    })
}

/// Backspace between an empty pair, as in `(|)`, deletes both characters.
pub fn backspace_pair(text: &str, sel: Selection, quotes: &[char]) -> Option<Edit> {
    if !sel.is_empty() {
        return None;
    }
    let at = char_to_byte(text, sel.start);
    let before = text[..at].chars().next_back()?;
    let after = text[at..].chars().next()?;
    let pairs = closer_for(before) == Some(after) || (before == after && quotes.contains(&before));
    if !pairs {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..at - before.len_utf8()]);
    out.push_str(&text[at + after.len_utf8()..]);
    Some(Edit {
        text: out,
        selection: Selection::caret(sel.start - 1),
    })
}

/// Enter: a newline that keeps the current line's indentation, one level
/// deeper after an opening bracket (or `:` in Python-like languages). Between
/// a bracket pair, as in `{|}`, the closer moves to its own line.
pub fn newline(text: &str, sel: Selection, unit: &str, extension: &str) -> Edit {
    let start = char_to_byte(text, sel.start);
    let mut end = char_to_byte(text, sel.end);
    // Whitespace after the caret would end up at the start of the new line,
    // in front of its indentation, so drop it.
    end += text[end..].len() - text[end..].trim_start_matches([' ', '\t']).len();

    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_before = &text[line_start..start];
    let indent = &line_before[..leading_whitespace(line_before)];
    let trimmed = line_before.trim_end();
    let last = trimmed.chars().next_back();
    let opens = last.is_some_and(|c| closer_for(c).is_some())
        || (last == Some(':') && colon_opens_block(extension));
    let closes_pair = last
        .and_then(closer_for)
        .is_some_and(|close| text[end..].starts_with(close));

    // Keep the text before the caret, minus trailing whitespace (an indented
    // blank line stays empty rather than keeping its indentation).
    let mut insert = String::from(trimmed);
    insert.push('\n');
    insert.push_str(indent);
    if opens {
        insert.push_str(unit);
    }
    let caret = line_start + insert.len();
    if closes_pair {
        insert.push('\n');
        insert.push_str(indent);
    }

    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..line_start]);
    out.push_str(&insert);
    out.push_str(&text[end..]);
    Edit {
        selection: Selection::caret(byte_to_char(&out, caret)),
        text: out,
    }
}

/// How far bracket matching scans before giving up, in bytes.
const BRACKET_SCAN_LIMIT: usize = 200_000;

/// The bracket next to the caret and its partner, as character indices
/// (a test helper over [`matching_bracket_in`]).
#[cfg(test)]
pub fn matching_bracket(text: &str, caret: usize) -> Option<(usize, usize)> {
    matching_bracket_in(text, &ropey::Rope::from_str(text), caret)
}

/// The bracket next to the caret and its partner, as character indices.
///
/// Looks at the character before the caret first, then the one after it.
/// Brackets inside strings and comments are not treated specially. The rope
/// converts offsets, so this costs the bracket scan only.
pub fn matching_bracket_in(text: &str, rope: &ropey::Rope, caret: usize) -> Option<(usize, usize)> {
    let byte = rope.char_to_byte(caret.min(rope.len_chars()));
    let bytes = text.as_bytes();
    for at in [byte.checked_sub(1), Some(byte)].into_iter().flatten() {
        let Some(&b) = bytes.get(at) else { continue };
        let partner = match b {
            b'(' | b'[' | b'{' => scan_forward(bytes, at),
            b')' | b']' | b'}' => scan_backward(bytes, at),
            _ => continue,
        };
        return partner.map(|p| (rope.byte_to_char(at), rope.byte_to_char(p)));
    }
    None
}

fn pair(b: u8) -> u8 {
    match b {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        b')' => b'(',
        b']' => b'[',
        _ => b'{',
    }
}

fn scan_forward(bytes: &[u8], at: usize) -> Option<usize> {
    let (open, close) = (bytes[at], pair(bytes[at]));
    let mut depth = 0usize;
    let end = bytes.len().min(at + BRACKET_SCAN_LIMIT);
    for (i, &b) in bytes.iter().enumerate().take(end).skip(at + 1) {
        if b == open {
            depth += 1;
        } else if b == close {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

fn scan_backward(bytes: &[u8], at: usize) -> Option<usize> {
    let (close, open) = (bytes[at], pair(bytes[at]));
    let mut depth = 0usize;
    let start = at.saturating_sub(BRACKET_SCAN_LIMIT);
    for i in (start..at).rev() {
        let b = bytes[i];
        if b == close {
            depth += 1;
        } else if b == open {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLASH: CommentStyle = CommentStyle::Line("//");

    fn sel(a: usize, b: usize) -> Selection {
        Selection::new(a, b)
    }

    #[test]
    fn comment_and_uncomment_round_trip() {
        let text = "fn a() {\n    x();\n\n    y();\n}";
        // Select "    x();\n\n    y();"
        let all = sel(9, 26);
        let commented = toggle_comment(text, all, SLASH);
        assert_eq!(commented.text, "fn a() {\n    // x();\n\n    // y();\n}");
        let back = toggle_comment(&commented.text, commented.selection, SLASH);
        assert_eq!(back.text, text);
    }

    #[test]
    fn comment_moves_caret_with_text() {
        let text = "let a = 1;";
        let e = toggle_comment(text, Selection::caret(4), CommentStyle::Line("#"));
        assert_eq!(e.text, "# let a = 1;");
        assert_eq!(e.selection, Selection::caret(6));
        let back = toggle_comment(&e.text, e.selection, CommentStyle::Line("#"));
        assert_eq!(back.text, text);
        assert_eq!(back.selection, Selection::caret(4));
    }

    #[test]
    fn mixed_lines_get_commented() {
        let text = "// a\nb";
        let e = toggle_comment(text, sel(0, 6), SLASH);
        assert_eq!(e.text, "// // a\n// b");
    }

    #[test]
    fn block_comments() {
        let style = CommentStyle::Block("<!--", "-->");
        let e = toggle_comment("  <p>hi</p>", Selection::caret(3), style);
        assert_eq!(e.text, "  <!-- <p>hi</p> -->");
        let back = toggle_comment(&e.text, e.selection, style);
        assert_eq!(back.text, "  <p>hi</p>");
    }

    #[test]
    fn selection_ending_at_line_start_excludes_that_line() {
        let text = "a\nb\nc";
        let e = toggle_comment(text, sel(0, 2), SLASH);
        assert_eq!(e.text, "// a\nb\nc");
    }

    #[test]
    fn indent_and_outdent() {
        let text = "a\n\nb\n  c";
        let e = indent(text, sel(0, text.chars().count()), "  ");
        assert_eq!(e.text, "  a\n\n  b\n    c");
        assert_eq!(e.selection, sel(0, e.text.chars().count()));
        let back = outdent(&e.text, e.selection, "  ");
        assert_eq!(back.text, text);
        let tabbed = outdent("\t\tx", Selection::caret(3), "\t");
        assert_eq!(tabbed.text, "\tx");
        assert_eq!(tabbed.selection, Selection::caret(2));
    }

    #[test]
    fn indent_single_blank_line() {
        let e = indent("a\n\nb", Selection::caret(2), "\t");
        assert_eq!(e.text, "a\n\t\nb");
        assert_eq!(e.selection, Selection::caret(3));
    }

    #[test]
    fn duplicate_lines_selects_copy() {
        let e = duplicate_lines("one\ntwo\nthree", Selection::caret(5));
        assert_eq!(e.text, "one\ntwo\ntwo\nthree");
        assert_eq!(e.selection, Selection::caret(9));
        let last = duplicate_lines("a\nb", Selection::caret(3));
        assert_eq!(last.text, "a\nb\nb");
    }

    #[test]
    fn move_lines_up_and_down() {
        let text = "1\n2\n3\n4";
        let down = move_lines(text, sel(2, 5), true).unwrap();
        assert_eq!(down.text, "2\n3\n1\n4");
        assert_eq!(down.selection, sel(0, 3));
        let up = move_lines(text, Selection::caret(4), false).unwrap();
        assert_eq!(up.text, "1\n2\n4\n3");
        assert_eq!(up.selection, Selection::caret(6));
        assert!(move_lines(text, Selection::caret(0), true).is_none());
        assert!(move_lines(text, Selection::caret(7), false).is_none());
    }

    #[test]
    fn brackets_match_both_directions() {
        let text = "f(a[1], {b})";
        assert_eq!(matching_bracket(text, 2), Some((1, 11)));
        assert_eq!(matching_bracket(text, 12), Some((11, 1)));
        assert_eq!(matching_bracket(text, 3), Some((3, 5)));
        assert_eq!(matching_bracket(text, 0), None);
        assert_eq!(matching_bracket("(é)", 3), Some((2, 0)));
        assert_eq!(matching_bracket("(()", 1), None);
    }

    #[test]
    fn line_positions() {
        let text = "ab\ncde\nf";
        assert_eq!(line_start(text, 1), 0);
        assert_eq!(line_start(text, 2), 3);
        assert_eq!(line_start(text, 3), 7);
        assert_eq!(line_start(text, 99), 7);
        assert_eq!(line_col_to_char(text, 2, 3), 5);
        assert_eq!(line_col_to_char(text, 2, 99), 6);
    }

    #[test]
    fn detects_indent_unit() {
        assert_eq!(indent_unit("a\n  b\n    c"), "  ");
        assert_eq!(indent_unit("a\n\tb\n\tc\n  d"), "\t");
        assert_eq!(indent_unit("plain"), "    ");
    }

    const QUOTES: &[char] = &['"', '\''];

    /// Applies `f` to `text` with the caret at `|`, returning the result with
    /// `|` (or `[..]` for a selection) marking the new selection.
    fn typed(text: &str, f: impl Fn(&str, Selection) -> Option<Edit>) -> Option<String> {
        let (text, sel) = parse_marked(text);
        let e = f(&text, sel)?;
        let mut out: Vec<char> = e.text.chars().collect();
        if e.selection.is_empty() {
            out.insert(e.selection.start, '|');
        } else {
            out.insert(e.selection.end, ']');
            out.insert(e.selection.start, '[');
        }
        Some(out.into_iter().collect())
    }

    fn parse_marked(text: &str) -> (String, Selection) {
        if let Some(caret) = text.find('|') {
            let at = text[..caret].chars().count();
            return (text.replacen('|', "", 1), Selection::caret(at));
        }
        let a = text[..text.find('[').unwrap()].chars().count();
        let b = text[..text.find(']').unwrap()].chars().count() - 1;
        (text.replacen('[', "", 1).replacen(']', "", 1), sel(a, b))
    }

    fn ty(text: &str, ch: char) -> Option<String> {
        typed(text, |t, s| type_char(t, s, ch, QUOTES))
    }

    #[test]
    fn brackets_and_quotes_auto_close() {
        assert_eq!(ty("f|", '(').as_deref(), Some("f(|)"));
        assert_eq!(ty("x = |;", '[').as_deref(), Some("x = [|];"));
        assert_eq!(ty("say |", '"').as_deref(), Some("say \"|\""));
        // Not in front of a word, after a word character, or for other chars.
        assert_eq!(ty("|word", '('), None);
        assert_eq!(ty("it|", '\''), None);
        assert_eq!(ty("a|", 'b'), None);
        // Quotes the language doesn't pair are plain.
        assert_eq!(typed(" |", |t, s| type_char(t, s, '\'', &['"'])), None);
    }

    #[test]
    fn typing_a_closer_steps_over_it() {
        assert_eq!(ty("f(|)", ')').as_deref(), Some("f()|"));
        assert_eq!(ty("\"ab|\"", '"').as_deref(), Some("\"ab\"|"));
        // An unmatched closer is typed normally.
        assert_eq!(ty("x|)", ')'), None);
    }

    #[test]
    fn typing_wraps_selection() {
        assert_eq!(ty("a [bc] d", '(').as_deref(), Some("a ([bc]) d"));
        assert_eq!(ty("[é]", '"').as_deref(), Some("\"[é]\""));
        assert_eq!(ty("[bc]", 'x'), None);
    }

    #[test]
    fn backspace_deletes_empty_pair() {
        let bs = |t: &str| typed(t, |t, s| backspace_pair(t, s, QUOTES));
        assert_eq!(bs("f(|)").as_deref(), Some("f|"));
        assert_eq!(bs("'|'").as_deref(), Some("|"));
        assert_eq!(bs("f(|x)"), None);
        assert_eq!(bs("(|]"), None);
    }

    #[test]
    fn enter_keeps_and_adds_indentation() {
        let nl = |t: &str, ext: &str| typed(t, |t, s| Some(newline(t, s, "    ", ext))).unwrap();
        assert_eq!(nl("    let a = 1;|", "rs"), "    let a = 1;\n    |");
        assert_eq!(nl("fn f() {|", "rs"), "fn f() {\n    |");
        assert_eq!(nl("  fn f() {|}", "rs"), "  fn f() {\n      |\n  }");
        assert_eq!(nl("call(a,|  b)", "rs"), "call(a,\n|b)");
        assert_eq!(nl("if x:|", "py"), "if x:\n    |");
        assert_eq!(nl("a:|", "rs"), "a:\n|");
        // A whitespace-only line doesn't keep its trailing spaces.
        assert_eq!(nl("x\n    |", "rs"), "x\n\n    |");
        // Replaces a selection.
        assert_eq!(nl("  a[bc]d", "rs"), "  a\n  |d");
    }

    #[test]
    fn insert_replaces_selection() {
        let e = insert("hello", sel(1, 4), "EY");
        assert_eq!(e.text, "hEYo");
        assert_eq!(e.selection, Selection::caret(3));
    }
}

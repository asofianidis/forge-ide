//! [EditorConfig](https://editorconfig.org) support: per-file indentation,
//! line endings and whitespace rules from `.editorconfig` files.
//!
//! Files are read from the edited file's folder upward, stopping at one that
//! says `root = true`. Closer files win, and within a file later sections win.

use std::ops::Range;
use std::path::Path;

use regex::Regex;

/// File name EditorConfig looks for.
pub const FILE_NAME: &str = ".editorconfig";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    Tab,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentSize {
    /// `indent_size = tab`: one indent is a tab, `tab_width` wide.
    Tab,
    Columns(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// The EditorConfig properties Forge understands, as they apply to one file.
/// `None` means unset: Forge falls back to its own behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorConfig {
    pub indent_style: Option<IndentStyle>,
    pub indent_size: Option<IndentSize>,
    pub tab_width: Option<usize>,
    pub end_of_line: Option<LineEnding>,
    pub trim_trailing_whitespace: Option<bool>,
    pub insert_final_newline: Option<bool>,
}

impl EditorConfig {
    /// The settings for `path`, from the `.editorconfig` files above it.
    /// Unreadable files are skipped.
    pub fn for_file(path: &Path) -> Self {
        let mut files = Vec::new();
        let mut dir = path.parent();
        while let Some(d) = dir {
            if let Ok(text) = std::fs::read_to_string(d.join(FILE_NAME)) {
                let file = ConfigFile::parse(&text);
                let root = file.root;
                files.push((d, file));
                if root {
                    break;
                }
            }
            dir = d.parent();
        }
        let mut config = Self::default();
        // Farthest first, so closer files override.
        for (dir, file) in files.iter().rev() {
            let Ok(rel) = path.strip_prefix(dir) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            for section in &file.sections {
                if section.glob.matches(&rel) {
                    for (key, value) in &section.properties {
                        config.set(key, value);
                    }
                }
            }
        }
        config
    }

    /// Applies one `key = value` pair (both already lowercased).
    fn set(&mut self, key: &str, value: &str) {
        let unset = value == "unset";
        let flag = || match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        match key {
            "indent_style" => {
                self.indent_style = match value {
                    "tab" => Some(IndentStyle::Tab),
                    "space" => Some(IndentStyle::Space),
                    _ if unset => None,
                    _ => return,
                }
            }
            "indent_size" => {
                self.indent_size = match value {
                    "tab" => Some(IndentSize::Tab),
                    _ if unset => None,
                    _ => match value.parse() {
                        Ok(n) if n > 0 => Some(IndentSize::Columns(n)),
                        _ => return,
                    },
                }
            }
            "tab_width" => {
                self.tab_width = match value.parse() {
                    Ok(n) if n > 0 => Some(n),
                    _ if unset => None,
                    _ => return,
                }
            }
            "end_of_line" => {
                self.end_of_line = match value {
                    "lf" => Some(LineEnding::Lf),
                    "crlf" => Some(LineEnding::CrLf),
                    // `cr` alone isn't supported: the editor only breaks lines at `\n`.
                    _ if unset => None,
                    _ => return,
                }
            }
            "trim_trailing_whitespace" => match flag() {
                Some(b) => self.trim_trailing_whitespace = Some(b),
                None if unset => self.trim_trailing_whitespace = None,
                None => {}
            },
            "insert_final_newline" => match flag() {
                Some(b) => self.insert_final_newline = Some(b),
                None if unset => self.insert_final_newline = None,
                None => {}
            },
            _ => {}
        }
    }

    /// Width of a tab character in columns, if configured.
    pub fn tab_width(&self) -> Option<usize> {
        self.tab_width.or(match self.indent_size {
            Some(IndentSize::Columns(n)) => Some(n),
            _ => None,
        })
    }

    /// The indentation to insert, falling back to what `detected` (the
    /// file's own indentation) uses for anything left unset.
    pub fn indent_unit(&self, detected: &str) -> String {
        let style = self.indent_style.or(match self.indent_size {
            Some(IndentSize::Tab) => Some(IndentStyle::Tab),
            _ => None,
        });
        let style = style.unwrap_or(if detected == "\t" {
            IndentStyle::Tab
        } else {
            IndentStyle::Space
        });
        match style {
            IndentStyle::Tab => "\t".to_owned(),
            IndentStyle::Space => {
                let width = match self.indent_size {
                    Some(IndentSize::Columns(n)) => n,
                    Some(IndentSize::Tab) => self.tab_width.unwrap_or(4),
                    None if detected == "\t" => self.tab_width.unwrap_or(4),
                    None => detected.len(),
                };
                " ".repeat(width.clamp(1, 16))
            }
        }
    }

    /// Whether saving would change anything.
    pub fn affects_save(&self) -> bool {
        self.end_of_line.is_some()
            || self.trim_trailing_whitespace == Some(true)
            || self.insert_final_newline.is_some()
    }

    /// Character edits that make `text` follow the line ending, trailing
    /// whitespace and final newline rules. Sorted and non-overlapping.
    pub fn save_edits(&self, text: &str) -> Vec<(Range<usize>, String)> {
        let mut edits = Vec::new();
        if !self.affects_save() {
            return edits;
        }
        // Line ending for an added final newline: the configured one, else
        // whatever the file's first line uses.
        let eol = self.end_of_line.unwrap_or(match text.find('\n') {
            Some(i) if text[..i].ends_with('\r') => LineEnding::CrLf,
            _ => LineEnding::Lf,
        });
        let trim = self.trim_trailing_whitespace == Some(true);

        let mut chars = 0; // char offset of the current line's start
        let mut lines = text.split('\n').peekable();
        while let Some(line) = lines.next() {
            let is_last = lines.peek().is_none();
            let (body, has_cr) = match line.strip_suffix('\r') {
                Some(b) if !is_last => (b, true),
                _ => (line, false),
            };
            let body_chars = body.chars().count();
            if trim {
                let kept = body.trim_end_matches([' ', '\t']);
                let kept_chars = kept.chars().count();
                if kept_chars < body_chars {
                    edits.push((chars + kept_chars..chars + body_chars, String::new()));
                }
            }
            if !is_last {
                let end = chars + body_chars;
                match (self.end_of_line, has_cr) {
                    (Some(LineEnding::Lf), true) => edits.push((end..end + 1, String::new())),
                    (Some(LineEnding::CrLf), false) => edits.push((end..end, "\r".to_owned())),
                    _ => {}
                }
            }
            chars += line.chars().count() + 1;
        }
        let total = chars - 1;

        match self.insert_final_newline {
            Some(true) if !text.is_empty() && !text.ends_with('\n') => {
                edits.push((total..total, eol.as_str().to_owned()));
            }
            Some(false) => {
                // Drop the trailing line breaks (and blank lines, when trimming),
                // along with any edits inside them.
                let kept = if trim {
                    text.trim_end_matches(['\n', '\r', ' ', '\t'])
                } else {
                    text.trim_end_matches(['\n', '\r'])
                };
                if kept.len() < text.len() {
                    let start = kept.chars().count();
                    edits.retain(|(r, _)| r.end <= start && !(r.is_empty() && r.start == start));
                    edits.push((start..total, String::new()));
                }
            }
            _ => {}
        }
        edits
    }
}

// ---- parsing ----------------------------------------------------------------

struct ConfigFile {
    root: bool,
    sections: Vec<Section>,
}

struct Section {
    glob: Glob,
    /// Lowercased keys and values, in file order.
    properties: Vec<(String, String)>,
}

impl ConfigFile {
    fn parse(text: &str) -> Self {
        let mut file = ConfigFile {
            root: false,
            sections: Vec::new(),
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(pattern) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                file.sections.push(Section {
                    glob: Glob::new(pattern),
                    properties: Vec::new(),
                });
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_lowercase();
            let value = value.trim().to_lowercase();
            match file.sections.last_mut() {
                Some(section) => section.properties.push((key, value)),
                None if key == "root" => file.root = value == "true",
                None => {}
            }
        }
        file
    }
}

/// An EditorConfig section glob, matched against a path relative to the
/// folder holding the `.editorconfig`, with `/` separators.
struct Glob {
    regex: Option<Regex>,
    /// Allowed values of each `{n1..n2}` group, in capture-group order.
    ranges: Vec<(i64, i64)>,
}

impl Glob {
    fn new(pattern: &str) -> Self {
        // A glob without a slash matches a file name in any folder; one with
        // a slash is relative to the `.editorconfig`'s folder.
        let (anchored, pattern) = match pattern.strip_prefix('/') {
            Some(p) => (true, p),
            None => (pattern.contains('/'), pattern),
        };
        let mut ranges = Vec::new();
        let chars: Vec<char> = pattern.chars().collect();
        let body = translate(&chars, &mut ranges);
        let prefix = if anchored { "^" } else { "^(?:.*/)?" };
        Glob {
            regex: Regex::new(&format!("{prefix}{body}$")).ok(),
            ranges,
        }
    }

    fn matches(&self, path: &str) -> bool {
        let Some(re) = &self.regex else {
            return false;
        };
        let Some(caps) = re.captures(path) else {
            return false;
        };
        self.ranges.iter().enumerate().all(|(i, &(lo, hi))| {
            caps.get(i + 1)
                .and_then(|m| m.as_str().parse::<i64>().ok())
                .is_some_and(|n| (lo..=hi).contains(&n))
        })
    }
}

/// Translates glob characters to a regex body.
fn translate(chars: &[char], ranges: &mut Vec<(i64, i64)>) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() => {
                out.push_str(&regex::escape(&chars[i + 1].to_string()));
                i += 2;
                continue;
            }
            '*' if chars.get(i + 1) == Some(&'*') => {
                out.push_str(".*");
                i += 2;
                continue;
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => {
                if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == ']' && j > i + 1)
                    && !chars[i + 1..end].contains(&'/')
                {
                    out.push('[');
                    let mut j = i + 1;
                    if matches!(chars[j], '!' | '^') {
                        out.push('^');
                        j += 1;
                    }
                    for &ch in &chars[j..end] {
                        if matches!(ch, '\\' | '[' | ']' | '^' | '&' | '~') {
                            out.push('\\');
                        }
                        out.push(ch);
                    }
                    out.push(']');
                    i = end + 1;
                    continue;
                }
                out.push_str("\\[");
            }
            '{' => {
                if let Some(end) = matching_brace(chars, i) {
                    let inner = &chars[i + 1..end];
                    let inner_str: String = inner.iter().collect();
                    if let Some(range) = numeric_range(&inner_str) {
                        ranges.push(range);
                        out.push_str("([+-]?[0-9]+)");
                    } else {
                        let alternatives = split_top_level(inner);
                        if alternatives.len() > 1 {
                            out.push_str("(?:");
                            for (k, alt) in alternatives.iter().enumerate() {
                                if k > 0 {
                                    out.push('|');
                                }
                                out.push_str(&translate(alt, ranges));
                            }
                            out.push(')');
                        } else {
                            // `{single}` is literal.
                            out.push_str("\\{");
                            out.push_str(&translate(inner, ranges));
                            out.push_str("\\}");
                        }
                    }
                    i = end + 1;
                    continue;
                }
                out.push_str("\\{");
            }
            _ => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    out
}

fn matching_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Splits brace contents at commas outside nested braces.
fn split_top_level(chars: &[char]) -> Vec<&[char]> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&chars[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&chars[start..]);
    parts
}

fn numeric_range(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.split_once("..")?;
    let (a, b) = (a.parse::<i64>().ok()?, b.parse::<i64>().ok()?);
    Some((a.min(b), a.max(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(p: &str, path: &str) -> bool {
        Glob::new(p).matches(path)
    }

    #[test]
    fn globs() {
        assert!(glob("*", "a.rs"));
        assert!(glob("*", "src/a.rs"));
        assert!(glob("*.rs", "src/deep/a.rs"));
        assert!(!glob("*.rs", "a.rs.bak"));
        assert!(glob("{*.js,*.ts}", "x/y.ts"));
        assert!(glob("*.{js,ts}", "y.js"));
        assert!(!glob("*.{js,ts}", "y.rs"));
        assert!(glob("Makefile", "sub/Makefile"));
        assert!(glob("src/*.rs", "src/a.rs"));
        assert!(!glob("src/*.rs", "x/src/a.rs"));
        assert!(!glob("src/*.rs", "src/b/a.rs"));
        assert!(glob("src/**.rs", "src/b/a.rs"));
        assert!(glob("/lib/**", "lib/x/y"));
        assert!(glob("a?.txt", "ab.txt"));
        assert!(glob("[abc].md", "b.md"));
        assert!(!glob("[!abc].md", "b.md"));
        assert!(glob("file{1..3}.txt", "file2.txt"));
        assert!(!glob("file{1..3}.txt", "file4.txt"));
        assert!(glob("{single}.txt", "{single}.txt"));
    }

    fn config(text: &str, rel: &str) -> EditorConfig {
        let file = ConfigFile::parse(text);
        let mut c = EditorConfig::default();
        for s in &file.sections {
            if s.glob.matches(rel) {
                for (k, v) in &s.properties {
                    c.set(k, v);
                }
            }
        }
        c
    }

    #[test]
    fn later_sections_win_and_unset_clears() {
        let text = "root = true\n# comment\n[*]\nindent_style = space\nindent_size = 4\n\
                    end_of_line = lf\n\n[*.go]\nindent_style = Tab\n\n[Makefile]\nindent_size = unset\n";
        assert!(ConfigFile::parse(text).root);
        let rs = config(text, "src/main.rs");
        assert_eq!(rs.indent_style, Some(IndentStyle::Space));
        assert_eq!(rs.indent_unit("  "), "    ");
        assert_eq!(rs.end_of_line, Some(LineEnding::Lf));
        let go = config(text, "main.go");
        assert_eq!(go.indent_unit("    "), "\t");
        assert_eq!(go.tab_width(), Some(4));
        let mk = config(text, "Makefile");
        assert_eq!(mk.indent_size, None);
        assert_eq!(mk.indent_unit("  "), "  ");
    }

    #[test]
    fn indent_unit_falls_back_to_detected() {
        let none = EditorConfig::default();
        assert_eq!(none.indent_unit("  "), "  ");
        assert_eq!(none.indent_unit("\t"), "\t");
        let size_only = EditorConfig {
            indent_size: Some(IndentSize::Columns(2)),
            ..Default::default()
        };
        assert_eq!(size_only.indent_unit("    "), "  ");
        assert_eq!(size_only.indent_unit("\t"), "\t");
        let tab_size = EditorConfig {
            indent_size: Some(IndentSize::Tab),
            tab_width: Some(8),
            ..Default::default()
        };
        assert_eq!(tab_size.indent_unit("  "), "\t");
        assert_eq!(tab_size.tab_width(), Some(8));
    }

    fn apply(text: &str, edits: &[(Range<usize>, String)]) -> String {
        let mut chars: Vec<char> = text.chars().collect();
        for (r, t) in edits.iter().rev() {
            chars.splice(r.clone(), t.chars());
        }
        chars.into_iter().collect()
    }

    fn saved(c: &EditorConfig, text: &str) -> String {
        let edits = c.save_edits(text);
        for w in edits.windows(2) {
            assert!(w[0].0.end <= w[1].0.start, "overlapping edits {edits:?}");
        }
        apply(text, &edits)
    }

    #[test]
    fn save_rules() {
        let trim = EditorConfig {
            trim_trailing_whitespace: Some(true),
            ..Default::default()
        };
        assert_eq!(saved(&trim, "a  \nbé\t\n  \nc "), "a\nbé\n\nc");
        assert_eq!(saved(&trim, "a \r\nb"), "a\r\nb");

        let final_nl = EditorConfig {
            insert_final_newline: Some(true),
            ..Default::default()
        };
        assert_eq!(saved(&final_nl, "a\nb"), "a\nb\n");
        assert_eq!(saved(&final_nl, "a\r\nb"), "a\r\nb\r\n");
        assert_eq!(saved(&final_nl, "a\n"), "a\n");
        assert_eq!(saved(&final_nl, ""), "");

        let no_final = EditorConfig {
            insert_final_newline: Some(false),
            trim_trailing_whitespace: Some(true),
            ..Default::default()
        };
        assert_eq!(saved(&no_final, "a \n\n \n"), "a");

        let lf = EditorConfig {
            end_of_line: Some(LineEnding::Lf),
            insert_final_newline: Some(true),
            ..Default::default()
        };
        assert_eq!(saved(&lf, "a\r\nb\r\nc"), "a\nb\nc\n");
        let crlf = EditorConfig {
            end_of_line: Some(LineEnding::CrLf),
            insert_final_newline: Some(true),
            ..Default::default()
        };
        assert_eq!(saved(&crlf, "a\nb\r\nc"), "a\r\nb\r\nc\r\n");
        assert!(EditorConfig::default().save_edits("a  ").is_empty());
    }

    #[test]
    fn reads_files_up_to_root() {
        let base = std::env::temp_dir().join(format!("forge-ec-{}", std::process::id()));
        let sub = base.join("proj").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join(FILE_NAME), "[*]\nindent_style = tab\n").unwrap();
        std::fs::write(
            base.join("proj").join(FILE_NAME),
            "root = true\n[*]\nindent_size = 2\n[*.md]\ntrim_trailing_whitespace = false\n",
        )
        .unwrap();
        std::fs::write(sub.join(FILE_NAME), "[*.rs]\nindent_size = 4\n").unwrap();
        let rs = EditorConfig::for_file(&sub.join("main.rs"));
        assert_eq!(rs.indent_size, Some(IndentSize::Columns(4)));
        // The outer file is above `root = true`, so it is ignored.
        assert_eq!(rs.indent_style, None);
        let md = EditorConfig::for_file(&sub.join("notes.md"));
        assert_eq!(md.indent_size, Some(IndentSize::Columns(2)));
        assert_eq!(md.trim_trailing_whitespace, Some(false));
        std::fs::remove_dir_all(base).unwrap();
    }
}

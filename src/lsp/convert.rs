//! Conversions between editor and LSP representations.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// `file://` URI for an absolute path, percent-encoding anything outside the
/// unreserved set (Windows paths become `file:///C:/...`).
pub fn path_to_uri(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') {
        s.insert(0, '/');
    }
    let mut out = String::from("file://");
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Path for a `file://` URI, or `None` for other schemes.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Drop an authority component (`file://localhost/...`).
    let rest = &rest[rest.find('/')?..];
    let decoded = percent_decode(rest)?;
    // `/C:/x` → `C:/x`
    let bytes = decoded.as_bytes();
    let path = if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        &decoded[1..]
    } else {
        &decoded[..]
    };
    Some(normalize(&PathBuf::from(if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_owned()
    })))
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = (bytes.next()? as char).to_digit(16)?;
            let lo = (bytes.next()? as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

/// Normalises a path for comparison: Windows drive letters are upper-cased
/// (servers often report `c:` where the editor has `C:`).
pub fn normalize(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_lowercase() && b[1] == b':' {
        let mut owned = s.into_owned();
        owned[..1].make_ascii_uppercase();
        PathBuf::from(owned)
    } else {
        path.to_path_buf()
    }
}

/// LSP `languageId` for a file extension.
pub fn language_id(extension: &str) -> &str {
    match extension {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "go" => "go",
        "md" => "markdown",
        "sh" | "bash" => "shellscript",
        other => other,
    }
}

/// Expands an LSP snippet to plain text: placeholders keep their default text,
/// choices keep their first option, and tab stops disappear.
///
/// Returns the text and the char range to select afterwards: the first tab stop
/// (`$1`, with its placeholder text selected so typing replaces it), else the
/// final stop `$0`, else the end of the text.
pub fn strip_snippet(snippet: &str) -> (String, Range<usize>) {
    let chars: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    let mut len = 0; // chars in `out`
    // Tab stops seen: (number, selected range).
    let mut stops: Vec<(u32, Range<usize>)> = Vec::new();
    // Placeholders whose closing `}` hasn't been reached: (number, start).
    let mut open: Vec<(u32, usize)> = Vec::new();

    let digits_at = |i: usize| {
        let end = (i..chars.len())
            .find(|&j| !chars[j].is_ascii_digit())
            .unwrap_or(chars.len());
        let n = chars[i..end].iter().collect::<String>().parse::<u32>().ok();
        n.map(|n| (n, end))
    };

    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                out.push(chars[i + 1]);
                len += 1;
                i += 2;
            }
            '$' => match (chars.get(i + 1), digits_at(i + 1)) {
                // $1
                (_, Some((n, end))) => {
                    stops.push((n, len..len));
                    i = end;
                }
                (Some('{'), _) => match digits_at(i + 2) {
                    Some((n, end)) => match chars.get(end) {
                        // ${1:placeholder}
                        Some(':') => {
                            open.push((n, len));
                            i = end + 1;
                        }
                        // ${1|one,two|}
                        Some('|') => {
                            let close = (end + 1..chars.len()).find(|&j| chars[j] == '|');
                            let body: String = chars[end + 1..close.unwrap_or(chars.len())]
                                .iter()
                                .collect();
                            let first = body.split(',').next().unwrap_or("");
                            let n_chars = first.chars().count();
                            stops.push((n, len..len + n_chars));
                            out.push_str(first);
                            len += n_chars;
                            i = close.map_or(chars.len(), |j| j + 2); // skip "|}"
                        }
                        // ${1}
                        _ => {
                            stops.push((n, len..len));
                            i = end + 1;
                        }
                    },
                    None => {
                        out.push('$');
                        len += 1;
                        i += 1;
                    }
                },
                _ => {
                    out.push('$');
                    len += 1;
                    i += 1;
                }
            },
            '}' if !open.is_empty() => {
                let (n, start) = open.pop().unwrap_or_default();
                stops.push((n, start..len));
                i += 1;
            }
            c => {
                out.push(c);
                len += 1;
                i += 1;
            }
        }
    }

    let selection = stops
        .iter()
        .filter(|(n, _)| *n > 0)
        .min_by_key(|(n, _)| *n)
        .or_else(|| stops.iter().find(|(n, _)| *n == 0))
        .map_or(len..len, |(_, r)| r.clone());
    (out, selection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_paths_round_trip() {
        let uri = path_to_uri(Path::new("/home/me/my project/a#b.rs"));
        assert_eq!(uri, "file:///home/me/my%20project/a%23b.rs");
        if !cfg!(windows) {
            assert_eq!(
                uri_to_path(&uri).unwrap(),
                Path::new("/home/me/my project/a#b.rs")
            );
        }
    }

    #[test]
    fn windows_drive_paths() {
        assert_eq!(
            path_to_uri(Path::new(r"C:\Code\x y.rs")),
            "file:///C:/Code/x%20y.rs"
        );
        let path = uri_to_path("file:///c%3A/Code/x%20y.rs").unwrap();
        let expected = if cfg!(windows) {
            r"C:\Code\x y.rs"
        } else {
            "C:/Code/x y.rs"
        };
        assert_eq!(path, PathBuf::from(expected));
    }

    #[test]
    fn non_file_uris_are_rejected() {
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
        assert_eq!(uri_to_path("file:///bad%zz"), None);
    }

    #[test]
    fn normalizes_drive_letter_case() {
        assert_eq!(normalize(Path::new("c:/x")), PathBuf::from("C:/x"));
        assert_eq!(normalize(Path::new("/c:/x")), PathBuf::from("/c:/x"));
    }

    #[test]
    fn snippets_keep_placeholders_and_select_first_stop() {
        assert_eq!(strip_snippet("foo"), ("foo".into(), 3..3));
        assert_eq!(strip_snippet("foo($1)$0"), ("foo()".into(), 4..4));
        assert_eq!(strip_snippet("foo()$0"), ("foo()".into(), 5..5));
        assert_eq!(
            strip_snippet("le(${1:other})$0"),
            ("le(other)".into(), 3..8)
        );
        assert_eq!(strip_snippet("f(${2:b}, ${1:a})"), ("f(b, a)".into(), 5..6));
        assert_eq!(strip_snippet("${1|a,b|} end"), ("a end".into(), 0..1));
        assert_eq!(strip_snippet(r"a\$1 \}"), ("a$1 }".into(), 5..5));
        assert_eq!(strip_snippet("vec![${1:${2:a}}]"), ("vec![a]".into(), 5..6));
        assert_eq!(strip_snippet("cost: $ 5"), ("cost: $ 5".into(), 9..9));
    }

    #[test]
    fn language_ids() {
        assert_eq!(language_id("rs"), "rust");
        assert_eq!(language_id("tsx"), "typescriptreact");
        assert_eq!(language_id("zig"), "zig");
    }
}

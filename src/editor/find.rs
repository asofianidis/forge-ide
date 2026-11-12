//! Find and replace within a single buffer.

use std::ops::Range;

use regex::{NoExpand, Regex, RegexBuilder};

/// Stop collecting matches after this many, so a one-letter query in a huge
/// file stays responsive.
pub const MAX_MATCHES: usize = 10_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FindOptions {
    pub match_case: bool,
    pub regex: bool,
    pub whole_word: bool,
}

/// One match, as byte offsets (for replacing) and char offsets (for the cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub bytes: Range<usize>,
    pub chars: Range<usize>,
}

/// Compiles `query` according to `options`. Plain-text queries are escaped.
pub fn build_regex(query: &str, options: FindOptions) -> Result<Regex, String> {
    let mut pattern = if options.regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    if options.whole_word {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    RegexBuilder::new(&pattern)
        .case_insensitive(!options.match_case)
        .multi_line(true)
        .build()
        .map_err(|e| match e {
            regex::Error::Syntax(s) => s.lines().last().unwrap_or("invalid regex").to_owned(),
            other => other.to_string(),
        })
}

/// All non-empty matches of `re` in `text`, in order, up to [`MAX_MATCHES`].
pub fn find_all(text: &str, re: &Regex) -> Vec<Match> {
    let mut out = Vec::new();
    let mut chars = 0;
    let mut last_byte = 0;
    for m in re
        .find_iter(text)
        .filter(|m| !m.is_empty())
        .take(MAX_MATCHES)
    {
        chars += text[last_byte..m.start()].chars().count();
        let len = m.as_str().chars().count();
        out.push(Match {
            bytes: m.range(),
            chars: chars..chars + len,
        });
        chars += len;
        last_byte = m.end();
    }
    out
}

/// The replacement text for the match at `m`. In regex mode `$1`/`${name}`
/// expand to capture groups; otherwise `replacement` is used literally.
fn expand(text: &str, re: &Regex, m: &Match, replacement: &str, regex: bool) -> String {
    if !regex {
        return replacement.to_owned();
    }
    let mut out = String::new();
    if let Some(caps) = re.captures_at(text, m.bytes.start) {
        caps.expand(replacement, &mut out);
    }
    out
}

/// Replaces a single match. Returns the new text and the char index just
/// after the inserted replacement.
pub fn replace_one(
    text: &str,
    re: &Regex,
    m: &Match,
    replacement: &str,
    regex: bool,
) -> (String, usize) {
    let with = expand(text, re, m, replacement, regex);
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(&text[..m.bytes.start]);
    out.push_str(&with);
    out.push_str(&text[m.bytes.end..]);
    (out, m.chars.start + with.chars().count())
}

/// Replaces every match. Returns the new text and the number of replacements.
pub fn replace_all(text: &str, re: &Regex, replacement: &str, regex: bool) -> (String, usize) {
    let count = re.find_iter(text).filter(|m| !m.is_empty()).count();
    if count == 0 {
        return (text.to_owned(), 0);
    }
    let out = if regex {
        re.replace_all(text, replacement).into_owned()
    } else {
        re.replace_all(text, NoExpand(replacement)).into_owned()
    };
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(match_case: bool, regex: bool) -> FindOptions {
        FindOptions {
            match_case,
            regex,
            whole_word: false,
        }
    }

    #[test]
    fn plain_text_is_escaped_and_case_insensitive_by_default() {
        let re = build_regex("a.b", FindOptions::default()).unwrap();
        let m = find_all("A.B axb a.b", &re);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].bytes, 8..11);
    }

    #[test]
    fn match_case_and_whole_word() {
        let re = build_regex("Foo", opts(true, false)).unwrap();
        assert_eq!(find_all("foo Foo", &re).len(), 1);
        let re = build_regex(
            "foo",
            FindOptions {
                whole_word: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(find_all("foo food foo", &re).len(), 2);
    }

    #[test]
    fn char_offsets_account_for_multibyte_text() {
        let re = build_regex("x", FindOptions::default()).unwrap();
        let m = find_all("éx éx", &re);
        assert_eq!(m[0].chars, 1..2);
        assert_eq!(m[1].chars, 4..5);
    }

    #[test]
    fn invalid_regex_reports_error() {
        assert!(build_regex("(", opts(false, true)).is_err());
    }

    #[test]
    fn replace_with_captures() {
        let re = build_regex(r"(\w+)@(\w+)", opts(false, true)).unwrap();
        let text = "a@b c@d";
        let m = find_all(text, &re);
        let (out, caret) = replace_one(text, &re, &m[1], "$2@$1", true);
        assert_eq!(out, "a@b d@c");
        assert_eq!(caret, 7);
        let (all, n) = replace_all(text, &re, "$2@$1", true);
        assert_eq!((all.as_str(), n), ("b@a d@c", 2));
    }

    #[test]
    fn literal_replacement_does_not_expand() {
        let re = build_regex("x", FindOptions::default()).unwrap();
        let (out, n) = replace_all("x x", &re, "$0", false);
        assert_eq!((out.as_str(), n), ("$0 $0", 2));
    }

    #[test]
    fn line_anchors_work_per_line() {
        let re = build_regex("^b", opts(false, true)).unwrap();
        assert_eq!(find_all("a\nb\nb", &re).len(), 2);
    }
}

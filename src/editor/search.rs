//! Find / replace over a document's text.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub pattern: String,
    pub case_sensitive: bool,
    pub regex: bool,
}

/// A compiled [`Query`].
pub struct Matcher {
    re: Regex,
    /// Whether replacement text may reference capture groups (`$1`, `${name}`).
    expand: bool,
}

impl Matcher {
    /// Compiles a query. Returns `Ok(None)` for an empty pattern and `Err` for an
    /// invalid regex.
    pub fn new(query: &Query) -> Result<Option<Self>, regex::Error> {
        if query.pattern.is_empty() {
            return Ok(None);
        }
        let source = if query.regex {
            query.pattern.clone()
        } else {
            regex::escape(&query.pattern)
        };
        let re = RegexBuilder::new(&source)
            .case_insensitive(!query.case_sensitive)
            .multi_line(true)
            .build()?;
        Ok(Some(Self {
            re,
            expand: query.regex,
        }))
    }

    /// Byte ranges of all non-empty matches in `text`.
    pub fn find_all(&self, text: &str) -> Vec<Range<usize>> {
        self.re
            .find_iter(text)
            .filter(|m| !m.is_empty())
            .map(|m| m.range())
            .collect()
    }

    /// The text that replaces the match at `range` (a byte range from [`Self::find_all`]).
    pub fn replacement(&self, text: &str, range: Range<usize>, replace_with: &str) -> String {
        if !self.expand {
            return replace_with.to_owned();
        }
        // Re-run the match anchored at the start so capture groups are available.
        match self.re.captures_at(text, range.start) {
            Some(caps) if caps.get(0).map(|m| m.range()) == Some(range) => {
                let mut out = String::new();
                caps.expand(replace_with, &mut out);
                out
            }
            _ => replace_with.to_owned(),
        }
    }
}

/// Index of the first match starting at or after `offset`, wrapping to the first match.
pub fn next_match(matches: &[Range<usize>], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let idx = matches.partition_point(|m| m.start < offset);
    Some(if idx == matches.len() { 0 } else { idx })
}

/// Index of the last match starting before `offset`, wrapping to the last match.
pub fn prev_match(matches: &[Range<usize>], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let idx = matches.partition_point(|m| m.start < offset);
    Some(if idx == 0 { matches.len() - 1 } else { idx - 1 })
}

/// Parses a go-to-line target: `"42"` or `"42:7"` (1-based line and optional column).
pub fn parse_line_col(input: &str) -> Option<(usize, Option<usize>)> {
    let input = input.trim();
    let (line, col) = match input.split_once([':', ',']) {
        Some((l, c)) => (l, Some(c.trim().parse().ok()?)),
        None => (input, None),
    };
    let line: usize = line.trim().parse().ok()?;
    (line > 0 && col != Some(0)).then_some((line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(pattern: &str, case_sensitive: bool, regex: bool) -> Matcher {
        Matcher::new(&Query {
            pattern: pattern.into(),
            case_sensitive,
            regex,
        })
        .unwrap()
        .unwrap()
    }

    #[test]
    fn plain_search_escapes_regex_syntax() {
        let m = matcher("a.b", true, false);
        assert_eq!(m.find_all("axb a.b"), vec![4..7]);
    }

    #[test]
    fn case_insensitive_by_default_flag() {
        let m = matcher("foo", false, false);
        assert_eq!(m.find_all("Foo fOO foo").len(), 3);
        let m = matcher("foo", true, false);
        assert_eq!(m.find_all("Foo fOO foo"), vec![8..11]);
    }

    #[test]
    fn empty_pattern_and_invalid_regex() {
        assert!(Matcher::new(&Query::default()).unwrap().is_none());
        let bad = Query {
            pattern: "(".into(),
            regex: true,
            ..Default::default()
        };
        assert!(Matcher::new(&bad).is_err());
    }

    #[test]
    fn regex_replacement_expands_captures() {
        let m = matcher(r"(\w+)=(\w+)", true, true);
        let text = "a=1 b=2";
        let ranges = m.find_all(text);
        assert_eq!(m.replacement(text, ranges[1].clone(), "$2=$1"), "2=b");
        let plain = matcher("a", true, false);
        assert_eq!(plain.replacement("a", 0..1, "$1"), "$1");
    }

    #[test]
    fn regex_anchors_are_per_line() {
        let m = matcher("^x", true, true);
        assert_eq!(m.find_all("x\nx\n").len(), 2);
    }

    #[test]
    fn next_and_prev_wrap_around() {
        let matches = vec![2..3, 5..6, 9..10];
        assert_eq!(next_match(&matches, 0), Some(0));
        assert_eq!(next_match(&matches, 3), Some(1));
        assert_eq!(next_match(&matches, 10), Some(0));
        assert_eq!(prev_match(&matches, 5), Some(0));
        assert_eq!(prev_match(&matches, 2), Some(2));
        assert_eq!(next_match(&[], 0), None);
    }

    #[test]
    fn parses_line_and_column() {
        assert_eq!(parse_line_col("42"), Some((42, None)));
        assert_eq!(parse_line_col(" 42:7 "), Some((42, Some(7))));
        assert_eq!(parse_line_col("3,1"), Some((3, Some(1))));
        assert_eq!(parse_line_col("0"), None);
        assert_eq!(parse_line_col("4:0"), None);
        assert_eq!(parse_line_col("x"), None);
        assert_eq!(parse_line_col(""), None);
    }
}

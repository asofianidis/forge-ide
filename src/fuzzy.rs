//! Small fuzzy matcher for quick open and the command palette.
//!
//! A candidate matches when every query character appears in it in order
//! (case-insensitively). Matches score higher when characters are consecutive,
//! start words or path segments, or fall in the file name.

/// A scored match with the char indices of the matched characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    pub indices: Vec<usize>,
}

fn is_separator(c: char) -> bool {
    matches!(c, '/' | '\\' | '_' | '-' | '.' | ' ' | ':')
}

/// Scores `candidate` against `query`, or `None` if it doesn't match.
/// An empty query matches everything with score 0.
pub fn score(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }
    let chars: Vec<char> = candidate.chars().collect();
    let name_start = chars
        .iter()
        .rposition(|&c| c == '/' || c == '\\')
        .map_or(0, |i| i + 1);

    // Try aligning within the file name first, then across the whole path,
    // and keep whichever scores better.
    let mut best: Option<FuzzyMatch> = None;
    for from in [name_start, 0] {
        if let Some(mut indices) = align(&chars, &query, from) {
            tighten(&chars, &query, &mut indices);
            let m = FuzzyMatch {
                score: score_alignment(&chars, name_start, &indices),
                indices,
            };
            if best.as_ref().is_none_or(|b| m.score > b.score) {
                best = Some(m);
            }
        }
        if from == 0 {
            break;
        }
    }
    best
}

/// Greedy left-to-right alignment of `query` in `chars`, starting at `from`.
fn align(chars: &[char], query: &[char], from: usize) -> Option<Vec<usize>> {
    let mut indices = Vec::with_capacity(query.len());
    let mut qi = 0;
    for (i, &c) in chars.iter().enumerate().skip(from) {
        if qi == query.len() {
            break;
        }
        if c.to_lowercase().eq(std::iter::once(query[qi])) {
            indices.push(i);
            qi += 1;
        }
    }
    (qi == query.len()).then_some(indices)
}

fn score_alignment(chars: &[char], name_start: usize, indices: &[usize]) -> i32 {
    let mut score = 0i32;
    let mut prev: Option<usize> = None;
    for &i in indices {
        score += 1;
        if prev.is_some_and(|p| p + 1 == i) {
            score += 8;
        }
        let at_word_start = i == 0
            || is_separator(chars[i - 1])
            || (chars[i].is_uppercase() && chars[i - 1].is_lowercase());
        if at_word_start {
            score += 6;
        }
        if i >= name_start {
            score += 3;
        }
        prev = Some(i);
    }
    if indices.first() == Some(&name_start) {
        score += 10;
    }
    // Prefer shorter candidates among equals.
    score -= (chars.len() / 8) as i32;
    score
}

/// Moves matched positions right-to-left to sit next to the following match
/// where possible, which reads better than the raw greedy alignment.
fn tighten(chars: &[char], query: &[char], indices: &mut [usize]) {
    for k in (0..indices.len().saturating_sub(1)).rev() {
        let next = indices[k + 1];
        if indices[k] + 1 == next {
            continue;
        }
        let want = query[k];
        if next > 0 && chars[next - 1].to_lowercase().eq(std::iter::once(want)) {
            indices[k] = next - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_all_chars_in_order() {
        assert!(score("abc", "a_b_c").is_some());
        assert!(score("acb", "abc").is_none());
        assert!(score("", "anything").is_some());
    }

    #[test]
    fn case_insensitive() {
        assert!(score("APP", "src/app.rs").is_some());
    }

    #[test]
    fn prefers_file_name_and_word_starts() {
        let name = score("app", "src/app.rs").unwrap().score;
        let path = score("app", "apple/pie/x.rs").unwrap().score;
        assert!(name > path, "{name} <= {path}");
        let start = score("gut", "src/editor/gutter.rs").unwrap().score;
        let middle = score("gut", "src/regular_utils.rs").unwrap().score;
        assert!(start > middle);
    }

    #[test]
    fn aligns_within_file_name() {
        let m = score("fbar", "src/app/find_bar.rs").unwrap();
        assert_eq!(m.indices, vec![8, 13, 14, 15]);
    }

    #[test]
    fn indices_point_at_matched_chars() {
        let m = score("mr", "src/main.rs").unwrap();
        let chars: Vec<char> = "src/main.rs".chars().collect();
        assert_eq!(m.indices.len(), 2);
        assert_eq!(chars[m.indices[0]].to_ascii_lowercase(), 'm');
        assert_eq!(chars[m.indices[1]].to_ascii_lowercase(), 'r');
    }
}

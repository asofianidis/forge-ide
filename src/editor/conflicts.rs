//! Finding and resolving git merge conflict blocks in a buffer:
//!
//! ```text
//! <<<<<<< HEAD          (current: our side)
//! ...
//! ||||||| base          (optional, with merge.conflictStyle=diff3/zdiff3)
//! ...
//! =======
//! ...
//! >>>>>>> feature       (incoming: their side)
//! ```

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// 0-based line of `<<<<<<<`.
    pub start: usize,
    /// 0-based line of `|||||||`, if the file uses diff3 style.
    pub base: Option<usize>,
    /// 0-based line of `=======`.
    pub mid: usize,
    /// 0-based line of `>>>>>>>`.
    pub end: usize,
    /// Text after `<<<<<<<` (usually `HEAD`).
    pub current_label: String,
    /// Text after `>>>>>>>` (usually the merged branch).
    pub incoming_label: String,
}

impl Conflict {
    /// 0-based lines of our side.
    pub fn current(&self) -> std::ops::Range<usize> {
        self.start + 1..self.base.unwrap_or(self.mid)
    }

    /// 0-based lines of their side.
    pub fn incoming(&self) -> std::ops::Range<usize> {
        self.mid + 1..self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Current,
    Incoming,
    Both,
}

fn marker<'a>(line: &'a str, m: &str) -> Option<&'a str> {
    let rest = line.trim_end_matches('\r').strip_prefix(m)?;
    // The marker is exactly seven characters, then a space or end of line.
    match rest.chars().next() {
        None => Some(""),
        Some(' ') => Some(rest.trim()),
        Some(_) => None,
    }
}

/// All complete conflict blocks in `text`, in order.
pub fn find(text: &str) -> Vec<Conflict> {
    let mut out = Vec::new();
    let mut open: Option<(usize, String)> = None;
    let mut base = None;
    let mut mid = None;
    for (i, line) in text.split('\n').enumerate() {
        if let Some(label) = marker(line, "<<<<<<<") {
            open = Some((i, label.to_owned()));
            base = None;
            mid = None;
        } else if open.is_some() && mid.is_none() && marker(line, "|||||||").is_some() {
            base = Some(i);
        } else if open.is_some() && mid.is_none() && marker(line, "=======") == Some("") {
            mid = Some(i);
        } else if let Some(label) = marker(line, ">>>>>>>")
            && let (Some((start, current_label)), Some(m)) = (open.take(), mid.take())
        {
            out.push(Conflict {
                start,
                base: base.take(),
                mid: m,
                end: i,
                current_label,
                incoming_label: label.to_owned(),
            });
        }
    }
    out
}

/// Replaces conflict `c` in `text` with the chosen side(s).
pub fn resolve(text: &str, c: &Conflict, how: Resolution) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let keep: Vec<&str> = match how {
        Resolution::Current => lines[c.current()].to_vec(),
        Resolution::Incoming => lines[c.incoming()].to_vec(),
        Resolution::Both => {
            let mut v = lines[c.current()].to_vec();
            v.extend_from_slice(&lines[c.incoming()]);
            v
        }
    };
    let mut out: Vec<&str> = lines[..c.start].to_vec();
    out.extend(keep);
    out.extend_from_slice(&lines[c.end + 1..]);
    out.join("\n")
}

/// Resolves every conflict the same way.
pub fn resolve_all(text: &str, how: Resolution) -> String {
    let mut text = text.to_owned();
    // Last first, so earlier line numbers stay valid.
    for c in find(&text).iter().rev() {
        text = resolve(&text, c, how);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "top\n<<<<<<< HEAD\nours\n=======\ntheirs 1\ntheirs 2\n>>>>>>> feature\nmid\n\
                          <<<<<<< HEAD\na\n||||||| base\nb\n=======\nc\n>>>>>>> other\nend\n";

    #[test]
    fn finds_conflicts_including_diff3() {
        let c = find(SAMPLE);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].start, c[0].mid, c[0].end), (1, 3, 6));
        assert_eq!(c[0].current(), 2..3);
        assert_eq!(c[0].incoming(), 4..6);
        assert_eq!(c[0].current_label, "HEAD");
        assert_eq!(c[0].incoming_label, "feature");
        assert_eq!(c[1].base, Some(10));
        assert_eq!(c[1].current(), 9..10);
        // Not markers: too long, or a markdown rule.
        assert!(find("<<<<<<<< x\n========\n>>>>>>> y\n").is_empty());
    }

    #[test]
    fn resolves_each_way() {
        let c = &find(SAMPLE)[0];
        assert!(resolve(SAMPLE, c, Resolution::Current).starts_with("top\nours\nmid\n"));
        assert!(
            resolve(SAMPLE, c, Resolution::Incoming).starts_with("top\ntheirs 1\ntheirs 2\nmid\n")
        );
        assert!(
            resolve(SAMPLE, c, Resolution::Both)
                .starts_with("top\nours\ntheirs 1\ntheirs 2\nmid\n")
        );
        assert_eq!(
            resolve_all(SAMPLE, Resolution::Incoming),
            "top\ntheirs 1\ntheirs 2\nmid\nc\nend\n"
        );
        assert_eq!(
            resolve_all(SAMPLE, Resolution::Current),
            "top\nours\nmid\na\nend\n"
        );
    }
}

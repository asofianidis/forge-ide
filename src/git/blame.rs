//! Parsing `git blame --porcelain`.

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub hash: String,
    pub author: String,
    pub email: String,
    /// Unix seconds.
    pub time: i64,
    pub summary: String,
}

impl BlameCommit {
    /// Lines changed in the buffer or working tree but not committed.
    pub fn is_uncommitted(&self) -> bool {
        self.hash.bytes().all(|b| b == b'0')
    }

    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(7)]
    }
}

/// The commit for each line of the file (index 0 = line 1).
pub type Blame = Vec<Arc<BlameCommit>>;

pub fn parse(out: &str) -> Blame {
    let mut commits: HashMap<String, BlameCommit> = HashMap::new();
    let mut shared: HashMap<String, Arc<BlameCommit>> = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in out.lines() {
        if line.starts_with('\t') {
            lines.push(current.clone());
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            current = key.to_owned();
            commits
                .entry(current.clone())
                .or_insert_with(|| BlameCommit {
                    hash: current.clone(),
                    author: String::new(),
                    email: String::new(),
                    time: 0,
                    summary: String::new(),
                });
            continue;
        }
        if let Some(c) = commits.get_mut(&current) {
            match key {
                "author" => c.author = value.to_owned(),
                "author-mail" => c.email = value.trim_matches(['<', '>']).to_owned(),
                "author-time" => c.time = value.parse().unwrap_or(0),
                "summary" => c.summary = value.to_owned(),
                _ => {}
            }
        }
    }
    lines
        .into_iter()
        .map(|hash| {
            shared
                .entry(hash.clone())
                .or_insert_with(|| Arc::new(commits.get(&hash).cloned().expect("header seen")))
                .clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_with_repeated_commits() {
        let a = "a".repeat(40);
        let z = "0".repeat(40);
        let out = format!(
            "{a} 1 1 2\nauthor Ada\nauthor-mail <ada@x>\nauthor-time 100\nauthor-tz +0000\n\
             summary First commit\nfilename f.rs\n\tline one\n\
             {a} 2 2\n\tline two\n\
             {z} 3 3 1\nauthor Not Committed Yet\nauthor-mail <not.committed.yet>\n\
             author-time 200\nsummary Version of f.rs from f.rs\nfilename f.rs\n\tnew line\n"
        );
        let blame = parse(&out);
        assert_eq!(blame.len(), 3);
        assert_eq!(blame[0].author, "Ada");
        assert_eq!(blame[0].email, "ada@x");
        assert_eq!(blame[0].summary, "First commit");
        assert!(Arc::ptr_eq(&blame[0], &blame[1]));
        assert!(blame[2].is_uncommitted());
        assert!(!blame[0].is_uncommitted());
        assert_eq!(blame[0].short(), "aaaaaaa");
    }
}

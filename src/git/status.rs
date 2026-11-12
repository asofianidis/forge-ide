//! Parsing `git status --porcelain=v2 --branch -z --show-stash`.

/// What happened to a file, in one of the two areas (index or work tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

impl Change {
    fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            b'M' => Self::Modified,
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'T' => Self::TypeChanged,
            b'U' => Self::Conflicted,
            _ => return None,
        })
    }

    /// One-letter badge shown next to the file name.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "U",
            Self::Conflicted => "!",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Added => "Added",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
            Self::Copied => "Copied",
            Self::TypeChanged => "Type changed",
            Self::Untracked => "Untracked",
            Self::Conflicted => "Conflict",
        }
    }
}

/// One path reported by `git status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to the repository root, with `/` separators.
    pub path: String,
    /// Previous path for renames and copies.
    pub orig_path: Option<String>,
    /// Change staged in the index, if any.
    pub staged: Option<Change>,
    /// Change in the work tree not yet staged, if any.
    pub unstaged: Option<Change>,
    pub conflict: Option<ConflictKind>,
}

/// The two-letter conflict code for unmerged paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    BothModified,
    BothAdded,
    BothDeleted,
    AddedByUs,
    AddedByThem,
    DeletedByUs,
    DeletedByThem,
}

impl ConflictKind {
    fn from_xy(xy: &[u8]) -> Self {
        match xy {
            b"AA" => Self::BothAdded,
            b"DD" => Self::BothDeleted,
            b"AU" => Self::AddedByUs,
            b"UA" => Self::AddedByThem,
            b"DU" => Self::DeletedByUs,
            b"UD" => Self::DeletedByThem,
            _ => Self::BothModified,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::BothModified => "both modified",
            Self::BothAdded => "both added",
            Self::BothDeleted => "both deleted",
            Self::AddedByUs => "added by us",
            Self::AddedByThem => "added by them",
            Self::DeletedByUs => "deleted by us",
            Self::DeletedByThem => "deleted by them",
        }
    }
}

impl Entry {
    pub fn is_untracked(&self) -> bool {
        self.unstaged == Some(Change::Untracked)
    }

    /// The change most worth showing (conflicts, then work tree, then index).
    pub fn primary(&self) -> Change {
        if self.conflict.is_some() {
            Change::Conflicted
        } else {
            self.unstaged.or(self.staged).unwrap_or(Change::Modified)
        }
    }

    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Directory part of the path (empty at the root).
    pub fn dir(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(d, _)| d)
    }
}

/// Branch information from the `# branch.*` headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchInfo {
    /// Current branch, `None` when HEAD is detached.
    pub head: Option<String>,
    /// Abbreviated commit HEAD points at, `None` before the first commit.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

impl BranchInfo {
    /// Text for the branch indicator: the branch name or a short commit id.
    pub fn display(&self) -> String {
        match (&self.head, &self.oid) {
            (Some(h), _) => h.clone(),
            (None, Some(oid)) => format!("({})", &oid[..oid.len().min(8)]),
            (None, None) => "(no branch)".to_owned(),
        }
    }

    pub fn is_unborn(&self) -> bool {
        self.oid.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: BranchInfo,
    pub entries: Vec<Entry>,
    pub stash_count: u32,
    /// A rebase, merge, cherry-pick or revert waiting to be finished.
    pub operation: Option<Operation>,
}

/// A multi-step git operation that is paused (usually on conflicts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Rebase,
    Merge,
    CherryPick,
    Revert,
}

impl Operation {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Rebase => "Rebase",
            Self::Merge => "Merge",
            Self::CherryPick => "Cherry-pick",
            Self::Revert => "Revert",
        }
    }

    /// The `git` subcommand that continues or aborts it.
    pub fn command(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

impl Status {
    pub fn staged(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| e.conflict.is_none() && e.staged.is_some())
    }

    pub fn unstaged(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| e.conflict.is_none() && e.unstaged.is_some())
    }

    pub fn conflicts(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.conflict.is_some())
    }

    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Number of files with any change, for the activity bar badge.
    pub fn change_count(&self) -> usize {
        self.entries.len()
    }
}

/// Parses NUL-separated porcelain v2 output.
pub fn parse(output: &str) -> Status {
    let mut status = Status::default();
    let mut fields = output.split('\0');
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let (kind, rest) = record.split_at(1);
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        match kind {
            "#" => parse_header(rest, &mut status),
            "1" => {
                // XY sub mH mI mW hH hI path
                if let Some((xy, path)) = split_fields(rest, 7) {
                    status.entries.push(ordinary(xy, path, None));
                }
            }
            "2" => {
                // XY sub mH mI mW hH hI Xscore path, then origPath as its own field
                if let Some((xy, path)) = split_fields(rest, 8) {
                    let orig = fields.next().map(str::to_owned);
                    status.entries.push(ordinary(xy, path, orig));
                }
            }
            "u" => {
                // XY sub m1 m2 m3 mW h1 h2 h3 path
                if let Some((xy, path)) = split_fields(rest, 9) {
                    status.entries.push(Entry {
                        path: path.to_owned(),
                        orig_path: None,
                        staged: None,
                        unstaged: None,
                        conflict: Some(ConflictKind::from_xy(xy.as_bytes())),
                    });
                }
            }
            "?" => status.entries.push(Entry {
                path: rest.to_owned(),
                orig_path: None,
                staged: None,
                unstaged: Some(Change::Untracked),
                conflict: None,
            }),
            _ => {} // "!" ignored files and anything unknown
        }
    }
    status
}

/// Splits off `n` space-separated fields; returns the first (XY) and the
/// remainder (the path, which may itself contain spaces).
fn split_fields(rest: &str, n: usize) -> Option<(&str, &str)> {
    let mut parts = rest.splitn(n + 1, ' ');
    let xy = parts.next()?;
    let path = parts.nth(n - 1)?;
    Some((xy, path))
}

fn ordinary(xy: &str, path: &str, orig_path: Option<String>) -> Entry {
    let b = xy.as_bytes();
    Entry {
        path: path.to_owned(),
        orig_path,
        staged: b.first().copied().and_then(Change::from_code),
        unstaged: b.get(1).copied().and_then(Change::from_code),
        conflict: None,
    }
}

fn parse_header(rest: &str, status: &mut Status) {
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    let b = &mut status.branch;
    match key {
        "branch.oid" => b.oid = (value != "(initial)").then(|| value.to_owned()),
        "branch.head" => b.head = (value != "(detached)").then(|| value.to_owned()),
        "branch.upstream" => b.upstream = Some(value.to_owned()),
        "branch.ab" => {
            for part in value.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    b.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    b.behind = n.parse().unwrap_or(0);
                }
            }
        }
        "stash" => status.stash_count = value.parse().unwrap_or(0),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_entries() {
        let out = "# branch.oid 1234567890abcdef\0# branch.head main\0\
                   # branch.upstream origin/main\0# branch.ab +2 -1\0# stash 3\0\
                   1 M. N... 100644 100644 100644 aaa bbb src/a b.rs\0\
                   1 .M N... 100644 100644 100644 aaa bbb README.md\0\
                   1 MM N... 100644 100644 100644 aaa bbb both.rs\0\
                   2 R. N... 100644 100644 100644 aaa bbb R100 new name.rs\0old.rs\0\
                   u UU N... 100644 100644 100644 100644 a b c conflict.rs\0\
                   ? notes/todo.txt\0";
        let s = parse(out);
        assert_eq!(s.branch.head.as_deref(), Some("main"));
        assert_eq!(s.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.branch.ahead, s.branch.behind), (2, 1));
        assert_eq!(s.stash_count, 3);
        assert_eq!(s.entries.len(), 6);

        assert_eq!(s.entries[0].path, "src/a b.rs");
        assert_eq!(s.entries[0].staged, Some(Change::Modified));
        assert_eq!(s.entries[0].unstaged, None);
        assert_eq!(s.entries[0].dir(), "src");
        assert_eq!(s.entries[0].file_name(), "a b.rs");

        let renamed = &s.entries[3];
        assert_eq!(renamed.path, "new name.rs");
        assert_eq!(renamed.orig_path.as_deref(), Some("old.rs"));
        assert_eq!(renamed.staged, Some(Change::Renamed));

        assert_eq!(s.entries[4].conflict, Some(ConflictKind::BothModified));
        assert!(s.entries[5].is_untracked());

        assert_eq!(s.staged().count(), 3);
        assert_eq!(s.unstaged().count(), 3);
        assert_eq!(s.conflicts().count(), 1);
    }

    #[test]
    fn parses_unborn_and_detached() {
        let s = parse("# branch.oid (initial)\0# branch.head main\0");
        assert!(s.branch.is_unborn());
        assert_eq!(s.branch.display(), "main");

        let s = parse("# branch.oid abcdef1234567\0# branch.head (detached)\0");
        assert_eq!(s.branch.head, None);
        assert_eq!(s.branch.display(), "(abcdef12)");
    }
}

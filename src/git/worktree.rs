//! Parsing `git worktree list --porcelain` and naming new worktrees.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Commit checked out, `None` for a bare repository.
    pub head: Option<String>,
    /// Short branch name, `None` when detached or bare.
    pub branch: Option<String>,
    pub bare: bool,
    /// `Some(reason)` when locked; the reason may be empty.
    pub locked: Option<String>,
    /// `Some(reason)` when git would prune it (its folder is gone).
    pub prunable: Option<String>,
    /// The repository's main worktree (always listed first).
    pub main: bool,
    /// The worktree this window has open.
    pub current: bool,
}

impl Worktree {
    /// Folder name, shown as the worktree's title.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// Branch, or what is checked out when there is none.
    pub fn describe_head(&self) -> String {
        if self.bare {
            return "bare".to_owned();
        }
        match (&self.branch, &self.head) {
            (Some(b), _) => b.clone(),
            (None, Some(h)) => format!("detached at {}", &h[..h.len().min(7)]),
            (None, None) => "detached".to_owned(),
        }
    }
}

/// Parses porcelain output. `current_root` marks the open worktree.
pub fn parse(out: &str, current_root: Option<&Path>) -> Vec<Worktree> {
    let current = current_root.map(canonical);
    let mut list = Vec::new();
    let mut wt: Option<Worktree> = None;
    for line in out.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => {
                list.extend(wt.take());
                wt = Some(Worktree {
                    path: PathBuf::from(value),
                    ..Worktree::default()
                });
            }
            _ => {
                let Some(w) = wt.as_mut() else { continue };
                match key {
                    "HEAD" => w.head = Some(value.to_owned()),
                    "branch" => {
                        w.branch = Some(
                            value
                                .strip_prefix("refs/heads/")
                                .unwrap_or(value)
                                .to_owned(),
                        )
                    }
                    "bare" => w.bare = true,
                    "locked" => w.locked = Some(value.to_owned()),
                    "prunable" => w.prunable = Some(value.to_owned()),
                    _ => {}
                }
            }
        }
    }
    list.extend(wt);
    for (i, w) in list.iter_mut().enumerate() {
        w.main = i == 0;
        w.current = current.as_ref().is_some_and(|c| *c == canonical(&w.path));
    }
    list
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// A branch name made safe for use in a folder name.
pub fn folder_slug(branch: &str) -> String {
    let slug: String = branch
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches(|c| c == '-' || c == '.').to_owned()
}

/// Default location for a new worktree: a sibling of the main worktree
/// named `<repo>-<branch>`.
pub fn default_path(main: &Path, branch: &str) -> Option<PathBuf> {
    let slug = folder_slug(branch);
    if slug.is_empty() {
        return None;
    }
    let parent = main.parent()?;
    let repo = main.file_name()?.to_string_lossy();
    Some(parent.join(format!("{repo}-{slug}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_list() {
        let out = "worktree /src/app\nHEAD 1111111111111111\nbranch refs/heads/main\n\n\
                   worktree /src/app-feature\nHEAD 2222222222222222\nbranch refs/heads/feature/x\nlocked on usb drive\n\n\
                   worktree /src/app-old\nHEAD 3333333333333333\ndetached\nlocked\nprunable gitdir file points to non-existent location\n";
        let list = parse(out, Some(Path::new("/src/app-feature")));
        assert_eq!(list.len(), 3);
        assert!(list[0].main && !list[0].current);
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert!(list[1].current && !list[1].main);
        assert_eq!(list[1].branch.as_deref(), Some("feature/x"));
        assert_eq!(list[1].locked.as_deref(), Some("on usb drive"));
        assert_eq!(list[2].branch, None);
        assert_eq!(list[2].describe_head(), "detached at 3333333");
        assert_eq!(list[2].locked.as_deref(), Some(""));
        assert!(list[2].prunable.is_some());
        assert_eq!(list[1].name(), "app-feature");
    }

    #[test]
    fn parses_bare_main() {
        let list = parse("worktree /src/repo.git\nbare\n", None);
        assert!(list[0].bare);
        assert_eq!(list[0].describe_head(), "bare");
    }

    #[test]
    fn names_new_worktree_folders() {
        assert_eq!(folder_slug("feature/new thing"), "feature-new-thing");
        assert_eq!(
            default_path(Path::new("/src/app"), "fix/bug-12"),
            Some(PathBuf::from("/src/app-fix-bug-12"))
        );
        assert_eq!(default_path(Path::new("/src/app"), "  "), None);
    }
}

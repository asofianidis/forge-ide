//! Git worktrees for threads: each one gets its own branch and folder, so the
//! agent's edits never touch the working copy you have open.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::Cli;

/// Where a thread's worktree lives and what it is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// The worktree's root folder.
    pub path: PathBuf,
    pub branch: String,
    /// What the branch started from.
    pub base: String,
    /// Still on its placeholder name; renamed after the thread's title.
    #[serde(default)]
    pub auto_named: bool,
}

/// Prefix for branches Forge creates.
const BRANCH_PREFIX: &str = "forge/";

fn git(cwd: &Path) -> Result<Cli, String> {
    let exe = Cli::locate("git").ok_or("git was not found")?;
    Ok(Cli::new(exe, cwd.to_path_buf()).with_env("GIT_TERMINAL_PROMPT", "0"))
}

/// `~/.forge/worktrees` (or `$FORGE_WORKTREES`); each repository gets a
/// folder in it.
fn worktrees_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FORGE_WORKTREES").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".forge").join("worktrees"))
}

fn branch_exists(git: &Cli, branch: &str) -> bool {
    git.output(
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        None,
    )
    .is_ok_and(|o| o.success)
}

/// Creates a worktree for `project` (a folder in a repository) on a new
/// branch from `base` (the current branch when `None`). Returns it and the
/// folder in it that corresponds to `project`.
pub fn create(
    project: &Path,
    base: Option<&str>,
    id: &str,
) -> Result<(WorktreeInfo, PathBuf), String> {
    let git = git(project)?;
    let root = PathBuf::from(git.run(&["rev-parse", "--show-toplevel"])?.trim());
    let base = match base.filter(|b| !b.is_empty()) {
        Some(b) => b.to_owned(),
        None => {
            let head = git.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
            let head = head.trim();
            if head == "HEAD" {
                // Detached: start from the commit itself.
                git.run(&["rev-parse", "HEAD"])?.trim().to_owned()
            } else {
                head.to_owned()
            }
        }
    };
    let repo = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    let short: String = id
        .chars()
        .rev()
        .take(6)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let mut name = format!("thread-{short}");
    let mut n = 2;
    while branch_exists(&git, &format!("{BRANCH_PREFIX}{name}")) {
        name = format!("thread-{short}-{n}");
        n += 1;
    }
    let branch = format!("{BRANCH_PREFIX}{name}");
    let dir = worktrees_root()
        .ok_or("no home folder for worktrees")?
        .join(&repo)
        .join(&name);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    git.run(&[
        "worktree",
        "add",
        "-b",
        &branch,
        &dir.to_string_lossy(),
        &base,
    ])?;
    // Work in the same subfolder the project is in, if it isn't the root.
    let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let root_c = std::fs::canonicalize(&root).unwrap_or(root);
    let folder = match project.strip_prefix(&root_c) {
        Ok(sub) if !sub.as_os_str().is_empty() => dir.join(sub),
        _ => dir.clone(),
    };
    Ok((
        WorktreeInfo {
            path: dir,
            branch,
            base,
            auto_named: true,
        },
        folder,
    ))
}

/// Renames the worktree's branch after the thread's title. Returns the new
/// branch name.
pub fn rename_branch(wt: &WorktreeInfo, title: &str) -> Result<String, String> {
    let git = git(&wt.path)?;
    let slug = slug(title);
    if slug.is_empty() {
        return Err("the title has no usable characters".to_owned());
    }
    let mut branch = format!("{BRANCH_PREFIX}{slug}");
    let mut n = 2;
    while branch_exists(&git, &branch) {
        branch = format!("{BRANCH_PREFIX}{slug}-{n}");
        n += 1;
    }
    git.run(&["branch", "-m", &wt.branch, &branch])?;
    Ok(branch)
}

/// Removes the worktree folder (the branch is kept, so no commits are lost).
pub fn remove(project: &Path, wt: &WorktreeInfo) -> Result<(), String> {
    let git = git(project)?;
    git.run(&["worktree", "remove", "--force", &wt.path.to_string_lossy()])?;
    Ok(())
}

/// A branch-name-friendly version of `text`: lowercase words joined by `-`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Fix login redirect loop"), "fix-login-redirect-loop");
        assert_eq!(
            slug("  Explain src/ai/flow.rs!! "),
            "explain-src-ai-flow-rs"
        );
        assert_eq!(slug("…"), "");
        assert!(slug(&"word ".repeat(30)).len() <= 48);
    }

    /// Creates, renames and removes a worktree in a scratch repository.
    #[test]
    fn worktree_lifecycle() {
        let Some(exe) = Cli::locate("git") else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("forge-wt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("myrepo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let g = Cli::new(exe, repo.clone());
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
        ] {
            g.run(&args).unwrap();
        }
        std::fs::write(repo.join("sub/a.txt"), "a").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "init"]).unwrap();

        // Keep the test's worktrees out of the real home folder.
        unsafe { std::env::set_var("FORGE_WORKTREES", dir.join("wts")) };
        let (wt, folder) = create(&repo.join("sub"), None, "0123abc123").unwrap();
        assert_eq!(wt.branch, "forge/thread-abc123");
        assert_eq!(wt.base, "main");
        assert_eq!(
            wt.path,
            dir.join("wts").join("myrepo").join("thread-abc123")
        );
        assert!(
            folder.join("a.txt").is_file(),
            "agent works in the same subfolder"
        );

        let renamed = rename_branch(&wt, "Fix the thing").unwrap();
        assert_eq!(renamed, "forge/fix-the-thing");
        let head = Cli::new(Cli::locate("git").unwrap(), wt.path.clone())
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap();
        assert_eq!(head.trim(), "forge/fix-the-thing");

        let wt = WorktreeInfo {
            branch: renamed,
            ..wt
        };
        remove(&repo, &wt).unwrap();
        assert!(!wt.path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

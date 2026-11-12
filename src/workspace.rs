//! Workspace / project explorer: a lazily-loaded file tree rooted at a folder.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, bail};

use crate::lsp::catalog::ServerSpec;

/// Directory names hidden from the explorer.
pub const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules"];

#[derive(Debug)]
pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// `None` until the directory is expanded for the first time.
    children: Option<Vec<FileNode>>,
}

impl FileNode {
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let is_dir = path.is_dir();
        Self {
            path,
            name,
            is_dir,
            children: None,
        }
    }

    /// Returns the children of this directory, reading them from disk on first access.
    pub fn children(&mut self) -> &mut [FileNode] {
        self.children
            .get_or_insert_with(|| read_dir_sorted(&self.path))
            .as_mut_slice()
    }

    /// The loaded node at `path` (this node or a descendant), if any.
    pub fn find_mut(&mut self, path: &Path) -> Option<&mut FileNode> {
        if self.path == path {
            return Some(self);
        }
        if !path.starts_with(&self.path) {
            return None;
        }
        self.children
            .as_mut()?
            .iter_mut()
            .find_map(|c| c.find_mut(path))
    }

    /// Re-reads this directory's listing if it has been loaded. Entries that
    /// are still there keep their own loaded children.
    pub fn reload(&mut self) {
        let Some(old) = self.children.take() else {
            return;
        };
        let mut old: HashMap<PathBuf, FileNode> =
            old.into_iter().map(|n| (n.path.clone(), n)).collect();
        let mut fresh = read_dir_sorted(&self.path);
        for node in &mut fresh {
            if let Some(prev) = old.remove(&node.path)
                && prev.is_dir == node.is_dir
            {
                *node = prev;
            }
        }
        self.children = Some(fresh);
    }

    /// Re-reads every loaded directory in this subtree.
    pub fn reload_all(&mut self) {
        self.reload();
        if let Some(children) = &mut self.children {
            for child in children.iter_mut().filter(|c| c.is_dir) {
                child.reload_all();
            }
        }
    }
}

/// Checks a name typed for a new or renamed entry. `allow_nested` permits
/// `a/b/c.rs`, creating the folders in between.
fn check_name(name: &str, allow_nested: bool) -> anyhow::Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Enter a name");
    }
    if !allow_nested && (name.contains('/') || name.contains('\\')) {
        bail!("A name can't contain / or \\");
    }
    let path = Path::new(name);
    if path.is_absolute() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("\"{name}\" isn't a valid name");
    }
    Ok(name)
}

/// Creates an empty file (or a folder) named `name` inside `dir`, along with
/// any folders in between. Fails if it already exists.
pub fn create_entry(dir: &Path, name: &str, folder: bool) -> anyhow::Result<PathBuf> {
    let path = dir.join(check_name(name, true)?);
    if path.symlink_metadata().is_ok() {
        bail!("{} already exists", path.display());
    }
    if folder {
        std::fs::create_dir_all(&path)
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(drop)
    }
    .with_context(|| format!("failed to create {}", path.display()))?;
    Ok(path)
}

/// Renames `path` within its folder. Changing only the case of a name works
/// on case-insensitive file systems too.
pub fn rename_entry(path: &Path, new_name: &str) -> anyhow::Result<PathBuf> {
    let new_name = check_name(new_name, false)?;
    let parent = path.parent().context("can't rename the root folder")?;
    let target = parent.join(new_name);
    if target == path {
        return Ok(target);
    }
    // On case-insensitive file systems `target` "exists" when only the case
    // changes; that is still the same entry, so allow it.
    let same_entry = || {
        let a = std::fs::canonicalize(path).ok();
        a.is_some() && a == std::fs::canonicalize(&target).ok()
    };
    if target.symlink_metadata().is_ok() && !same_entry() {
        bail!("{} already exists", target.display());
    }
    std::fs::rename(path, &target)
        .with_context(|| format!("failed to rename {}", path.display()))?;
    Ok(target)
}

/// Where a path ends up after `from` is renamed to `to`: unchanged unless it
/// is `from` itself or inside it.
pub fn renamed_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

fn read_dir_sorted(dir: &Path) -> Vec<FileNode> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes: Vec<FileNode> = entries
        .filter_map(Result::ok)
        .map(|e| FileNode::new(e.path()))
        // `.git` is a file in worktrees and submodules, so hide it either way.
        .filter(|n| !((n.is_dir || n.name == ".git") && IGNORED_DIRS.contains(&n.name.as_str())))
        .collect();
    // Directories first, then case-insensitive alphabetical.
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    nodes
}

/// Limits for the file-type scan, so huge trees don't stall opening a folder.
const SCAN_MAX_DEPTH: usize = 5;
const SCAN_MAX_ENTRIES: usize = 5_000;

/// The kinds of files present in a project: lowercase extensions plus exact
/// file names (for things like `Dockerfile`).
#[derive(Debug, Default, Clone)]
pub struct FileKinds {
    extensions: HashSet<String>,
    filenames: HashSet<String>,
}

impl FileKinds {
    pub fn insert_file(&mut self, file_name: &str) {
        if let Some((_, ext)) = file_name.rsplit_once('.') {
            self.extensions.insert(ext.to_lowercase());
        }
        self.filenames.insert(file_name.to_owned());
    }

    /// Whether any file in the project would be handled by `spec`.
    pub fn uses(&self, spec: &ServerSpec) -> bool {
        self.extensions.iter().any(|e| spec.handles_extension(e))
            || spec.filenames.iter().any(|f| self.filenames.contains(*f))
    }

    /// Walks `root` (bounded in depth and size) collecting file kinds.
    pub fn scan(root: &Path) -> Self {
        let mut kinds = Self::default();
        let mut budget = SCAN_MAX_ENTRIES;
        let mut stack = vec![(root.to_path_buf(), 0)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                if budget == 0 {
                    return kinds;
                }
                budget -= 1;
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    if depth < SCAN_MAX_DEPTH
                        && !IGNORED_DIRS.contains(&name.as_str())
                        && (!name.starts_with('.') || name == ".github")
                    {
                        stack.push((entry.path(), depth + 1));
                    }
                } else {
                    kinds.insert_file(&name);
                }
            }
        }
        kinds
    }
}

pub struct Workspace {
    pub root: FileNode,
    /// File types found when the folder was opened.
    pub file_kinds: FileKinds,
}

impl Workspace {
    pub fn open(root: PathBuf) -> Self {
        Self {
            file_kinds: FileKinds::scan(&root),
            root: FileNode::new(root),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::catalog;

    #[test]
    fn file_kinds_match_servers() {
        let mut kinds = FileKinds::default();
        kinds.insert_file("main.RS");
        kinds.insert_file("Dockerfile");
        assert!(kinds.uses(catalog::find("rust-analyzer").unwrap()));
        assert!(kinds.uses(catalog::find("dockerfile-language-server").unwrap()));
        assert!(!kinds.uses(catalog::find("gopls").unwrap()));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-ws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_rename_and_validate_entries() {
        let dir = temp_dir("entries");
        let file = create_entry(&dir, " src/deep/a.rs ", false).unwrap();
        assert_eq!(file, dir.join("src/deep/a.rs"));
        assert!(file.is_file());
        assert!(create_entry(&dir, "src/deep/a.rs", false).is_err());
        assert!(create_entry(&dir, "../escape", false).is_err());
        assert!(create_entry(&dir, "", true).is_err());
        assert!(create_entry(&dir, "docs", true).unwrap().is_dir());

        assert!(rename_entry(&file, "b/c.rs").is_err());
        assert!(rename_entry(&file, "..").is_err());
        let renamed = rename_entry(&file, "b.rs").unwrap();
        assert!(renamed.is_file() && !file.exists());
        // Only the case changes: fine even where the file system ignores case.
        let upper = rename_entry(&renamed, "B.rs").unwrap();
        assert!(upper.is_file());
        create_entry(&dir, "src/deep/other.rs", false).unwrap();
        assert!(rename_entry(&upper, "other.rs").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn renamed_paths_follow_their_folder() {
        let from = Path::new("/w/src");
        let to = Path::new("/w/lib");
        assert_eq!(renamed_path(from, from, to).unwrap(), to);
        assert_eq!(
            renamed_path(Path::new("/w/src/a/b.rs"), from, to).unwrap(),
            Path::new("/w/lib/a/b.rs")
        );
        assert_eq!(renamed_path(Path::new("/w/srcx/a.rs"), from, to), None);
    }

    #[test]
    fn reload_keeps_loaded_subfolders() {
        let dir = temp_dir("reload");
        std::fs::create_dir_all(dir.join("a/inner")).unwrap();
        let mut root = FileNode::new(dir.clone());
        root.children();
        root.find_mut(&dir.join("a")).unwrap().children();
        assert!(root.find_mut(&dir.join("a/inner")).is_some());

        std::fs::write(dir.join("new.txt"), "").unwrap();
        std::fs::write(dir.join("a/later.txt"), "").unwrap();
        root.reload();
        let names: Vec<_> = root.children().iter().map(|n| n.name.clone()).collect();
        assert_eq!(names, ["a", "new.txt"]);
        // `a` kept its listing, so it doesn't show `later.txt` until reloaded.
        let a = root.find_mut(&dir.join("a")).unwrap();
        assert_eq!(a.children().len(), 1);
        root.reload_all();
        let a = root.find_mut(&dir.join("a")).unwrap();
        assert_eq!(a.children().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_finds_nested_files_and_skips_ignored_dirs() {
        let root = std::env::temp_dir().join(format!("forge-scan-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("src/lib.go"), "").unwrap();
        std::fs::write(root.join("node_modules/x.vue"), "").unwrap();

        let kinds = FileKinds::scan(&root);
        std::fs::remove_dir_all(&root).unwrap();

        assert!(kinds.uses(catalog::find("gopls").unwrap()));
        assert!(!kinds.uses(catalog::find("vue-language-server").unwrap()));
    }
}

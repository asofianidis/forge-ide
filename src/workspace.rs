//! Workspace / project explorer: a lazily-loaded file tree rooted at a folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::lsp::catalog::ServerSpec;

/// Directory names hidden from the explorer.
const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules"];

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

    /// Drops cached children so they are re-read on next expansion.
    pub fn refresh(&mut self) {
        self.children = None;
    }
}

fn read_dir_sorted(dir: &Path) -> Vec<FileNode> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes: Vec<FileNode> = entries
        .filter_map(Result::ok)
        .map(|e| FileNode::new(e.path()))
        .filter(|n| !(n.is_dir && IGNORED_DIRS.contains(&n.name.as_str())))
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
                        && !name.starts_with('.')
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

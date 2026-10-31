//! Workspace / project explorer: a lazily-loaded file tree rooted at a folder.

use std::path::{Path, PathBuf};

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

pub struct Workspace {
    pub root: FileNode,
}

impl Workspace {
    pub fn open(root: PathBuf) -> Self {
        Self {
            root: FileNode::new(root),
        }
    }
}

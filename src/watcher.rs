//! Watches the open folder for changes made by other programs, so the file
//! tree, open files and git status can follow them without polling.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::workspace::IGNORED_DIRS;

/// Wait this long after the last event before reacting, so a burst (a branch
/// switch, a formatter run) is handled once.
const QUIET: Duration = Duration::from_millis(150);
/// React at least this often during a long burst (e.g. a big build).
const MAX_DELAY: Duration = Duration::from_secs(1);

/// What changed since the last batch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Work tree paths that were created, removed, renamed or written.
    pub paths: HashSet<PathBuf>,
    /// Git metadata changed: a commit, checkout, fetch, stage, …
    pub git: bool,
    /// The OS dropped events, so anything may have changed.
    pub rescan: bool,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.git && !self.rescan
    }

    /// Whether `path` itself (or everything) changed.
    pub fn touches(&self, path: &Path) -> bool {
        self.rescan || self.paths.contains(path)
    }

    /// Folders whose listing may have changed.
    pub fn parent_dirs(&self) -> HashSet<&Path> {
        self.paths.iter().filter_map(|p| p.parent()).collect()
    }
}

/// How an event path matters to Forge.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Ignored,
    Git,
    /// A work tree path, given relative to the root.
    WorkTree(PathBuf),
}

/// Sorts `path` into work tree, git metadata or noise (build output,
/// dependencies, git's object store and lock files).
fn classify(roots: &[PathBuf], path: &Path) -> Kind {
    let Some(rel) = roots.iter().find_map(|r| path.strip_prefix(r).ok()) else {
        return Kind::Ignored;
    };
    let mut parts = rel.components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let Some(first) = parts.next() else {
        return Kind::Ignored;
    };
    if first == ".git" {
        let rest: Vec<&str> = parts.collect();
        let git_relevant = match rest.as_slice() {
            [] => true,
            [.., last] if last.ends_with(".lock") => false,
            ["refs", ..] | ["rebase-merge", ..] | ["rebase-apply", ..] => true,
            [file] => *file == "index" || *file == "packed-refs" || file.ends_with("HEAD"),
            _ => false,
        };
        return if git_relevant {
            Kind::Git
        } else {
            Kind::Ignored
        };
    }
    if std::iter::once(first)
        .chain(parts)
        .any(|p| IGNORED_DIRS.contains(&p))
    {
        return Kind::Ignored;
    }
    Kind::WorkTree(rel.to_path_buf())
}

enum Message {
    Path(PathBuf),
    Git,
    Rescan,
}

pub struct FsWatcher {
    root: PathBuf,
    rx: Receiver<Message>,
    pending: Changes,
    first_event: Option<Instant>,
    last_event: Option<Instant>,
    // Dropping the watcher stops it.
    _watcher: RecommendedWatcher,
}

impl FsWatcher {
    /// Starts watching `root` and everything below it.
    pub fn new(root: &Path, ctx: &egui::Context) -> notify::Result<Self> {
        // Events may name the folder by its canonical path (on macOS
        // `/var/…` arrives as `/private/var/…`), so match either spelling.
        let mut roots = vec![root.to_path_buf()];
        if let Ok(canonical) = root.canonicalize()
            && canonical != root
        {
            roots.push(canonical);
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let root_out = root.to_path_buf();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let messages: Vec<Message> = match res {
                    Ok(event) if event.need_rescan() => vec![Message::Rescan],
                    Ok(event) if matches!(event.kind, EventKind::Access(_)) => Vec::new(),
                    Ok(event) => event
                        .paths
                        .iter()
                        .filter_map(|p| match classify(&roots, p) {
                            Kind::Ignored => None,
                            Kind::Git => Some(Message::Git),
                            Kind::WorkTree(rel) => Some(Message::Path(root_out.join(rel))),
                        })
                        .collect(),
                    Err(_) => vec![Message::Rescan],
                };
                // Ignored events (build output, …) never wake the UI.
                if !messages.is_empty() {
                    for m in messages {
                        let _ = tx.send(m);
                    }
                    ctx.request_repaint();
                }
            })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            root: root.to_path_buf(),
            rx,
            pending: Changes::default(),
            first_event: None,
            last_event: None,
            _watcher: watcher,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Collects events and returns them as one batch once they settle.
    /// Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Changes> {
        let now = Instant::now();
        for message in self.rx.try_iter() {
            match message {
                Message::Path(p) => {
                    self.pending.paths.insert(p);
                }
                Message::Git => self.pending.git = true,
                Message::Rescan => self.pending.rescan = true,
            }
            self.first_event.get_or_insert(now);
            self.last_event = Some(now);
        }
        if self.pending.is_empty() {
            return None;
        }
        let (Some(first), Some(last)) = (self.first_event, self.last_event) else {
            return None;
        };
        let wait = QUIET
            .saturating_sub(now - last)
            .min(MAX_DELAY.saturating_sub(now - first));
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return None;
        }
        self.first_event = None;
        self.last_event = None;
        Some(std::mem::take(&mut self.pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_paths() {
        let roots = [PathBuf::from("/w"), PathBuf::from("/private/w")];
        let c = |p: &str| classify(&roots, Path::new(p));
        assert_eq!(c("/w/src/a.rs"), Kind::WorkTree("src/a.rs".into()));
        assert_eq!(c("/private/w/a.rs"), Kind::WorkTree("a.rs".into()));
        assert_eq!(c("/elsewhere/a.rs"), Kind::Ignored);
        assert_eq!(c("/w"), Kind::Ignored);
        assert_eq!(c("/w/target/debug/x"), Kind::Ignored);
        assert_eq!(c("/w/web/node_modules/x/y.js"), Kind::Ignored);
        assert_eq!(c("/w/.git/HEAD"), Kind::Git);
        assert_eq!(c("/w/.git/ORIG_HEAD"), Kind::Git);
        assert_eq!(c("/w/.git/index"), Kind::Git);
        assert_eq!(c("/w/.git/index.lock"), Kind::Ignored);
        assert_eq!(c("/w/.git/refs/heads/main"), Kind::Git);
        assert_eq!(c("/w/.git/refs/heads/main.lock"), Kind::Ignored);
        assert_eq!(c("/w/.git/objects/ab/cdef"), Kind::Ignored);
        assert_eq!(c("/w/.git/logs/HEAD"), Kind::Ignored);
    }

    #[test]
    fn reports_changes_from_disk() {
        let dir = std::env::temp_dir().join(format!("forge-watch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = egui::Context::default();
        let mut watcher = FsWatcher::new(&dir, &ctx).unwrap();
        // Some backends (FSEvents) need a moment before they report.
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("new.txt"), "hi").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = Changes::default();
        while Instant::now() < deadline && !seen.touches(&dir.join("new.txt")) {
            std::thread::sleep(Duration::from_millis(50));
            if let Some(changes) = watcher.poll(&ctx) {
                seen.paths.extend(changes.paths);
                seen.rescan |= changes.rescan;
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(seen.touches(&dir.join("new.txt")), "{seen:?}");
    }
}

//! Several Forge windows at once. Every window is its own process, so one
//! window's language servers, terminals and git work never stall another.
//!
//! Windows find each other through small files in
//! `<config dir>/forge-ide/windows/`: each keeps `<pid>.json` (its title and
//! folder) fresh with a heartbeat, so the Window menu can list them all, and
//! picks up `<pid>.focus` when another window asks it to come to the front.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How often a window rewrites its file to show it's still running.
const HEARTBEAT: Duration = Duration::from_secs(2);
/// A file this old belongs to a window that crashed or was killed.
const STALE: Duration = Duration::from_secs(10);
/// How often a window checks whether another one asked it to come forward.
const TICK: Duration = Duration::from_millis(200);
/// How long the Window menu reuses its last look at the folder.
const LIST_CACHE: Duration = Duration::from_millis(500);

/// One running Forge window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub pid: u32,
    /// Window title, e.g. `forge-ide — Forge IDE`.
    pub title: String,
    /// Folder open in the window, if any.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// When the window started (ms since the Unix epoch), for menu order.
    #[serde(default)]
    pub started: u128,
}

impl WindowInfo {
    /// Name shown in the Window menu: the folder's name, or "Empty Window".
    pub fn name(&self) -> String {
        match &self.folder {
            Some(folder) => folder.file_name().map_or_else(
                || folder.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            None => "Empty Window".to_owned(),
        }
    }
}

/// This window's entry in the registry, and the others it can see.
pub struct Windows {
    dir: Option<PathBuf>,
    info: Arc<Mutex<WindowInfo>>,
    focus_requested: Arc<AtomicBool>,
    /// Dropping this stops the heartbeat thread.
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    cache: Option<(Instant, Vec<WindowInfo>)>,
    /// Ask the OS for focus once more, after un-minimizing has had a moment.
    refocus_at: Option<Instant>,
}

/// The folder where windows register themselves.
fn registry_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FORGE_WINDOWS_DIR").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    crate::theme::config_dir().map(|d| d.join("windows"))
}

fn entry_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.json"))
}

fn focus_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.focus"))
}

/// Writes `info` to its file, through a temporary file so other windows
/// never read half of it.
fn write_entry(dir: &Path, info: &WindowInfo) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{}.json.tmp", info.pid));
    std::fs::write(&tmp, serde_json::to_vec(info)?)?;
    std::fs::rename(&tmp, entry_path(dir, info.pid))
}

/// The windows registered in `dir` with a fresh heartbeat, oldest first.
/// Files left by windows that are gone are removed.
pub fn read_windows(dir: &Path) -> Vec<WindowInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut windows = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "json" && ext != "focus" && ext != "tmp" {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .unwrap_or_default();
        if age > STALE {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if ext != "json" {
            continue;
        }
        if let Some(info) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WindowInfo>(&bytes).ok())
        {
            windows.push(info);
        }
    }
    windows.sort_by_key(|w| (w.started, w.pid));
    windows
}

impl Windows {
    /// Registers this window and starts its heartbeat. `ctx` is woken when
    /// another window asks this one to come to the front.
    pub fn register(ctx: &egui::Context) -> Self {
        Self::register_in(registry_dir(), ctx)
    }

    fn register_in(dir: Option<PathBuf>, ctx: &egui::Context) -> Self {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let info = Arc::new(Mutex::new(WindowInfo {
            pid: std::process::id(),
            title: "Forge IDE".to_owned(),
            folder: None,
            started,
        }));
        let focus_requested = Arc::new(AtomicBool::new(false));
        let (stop, thread) = match &dir {
            Some(dir) => {
                let (tx, rx) = mpsc::channel::<()>();
                let dir = dir.clone();
                let info = info.clone();
                let focus = focus_requested.clone();
                let ctx = ctx.clone();
                let thread = std::thread::Builder::new()
                    .name("forge-windows".to_owned())
                    .spawn(move || {
                        let mut written: Option<(WindowInfo, Instant)> = None;
                        loop {
                            let current = info.lock().map(|i| i.clone()).ok();
                            if let Some(current) = current {
                                let due = written.as_ref().is_none_or(|(last, at)| {
                                    *last != current || at.elapsed() >= HEARTBEAT
                                });
                                if due && write_entry(&dir, &current).is_ok() {
                                    written = Some((current.clone(), Instant::now()));
                                }
                                if std::fs::remove_file(focus_path(&dir, current.pid)).is_ok() {
                                    focus.store(true, Ordering::SeqCst);
                                    ctx.request_repaint();
                                }
                            }
                            match rx.recv_timeout(TICK) {
                                Err(RecvTimeoutError::Timeout) => {}
                                _ => break,
                            }
                        }
                    })
                    .ok();
                (Some(tx), thread)
            }
            None => (None, None),
        };
        Self {
            dir,
            info,
            focus_requested,
            stop,
            thread,
            cache: None,
            refocus_at: None,
        }
    }

    pub fn pid(&self) -> u32 {
        std::process::id()
    }

    /// Updates the title and folder other windows see for this one.
    pub fn set(&mut self, title: &str, folder: Option<&Path>) {
        if let Ok(mut info) = self.info.lock()
            && (info.title != title || info.folder.as_deref() != folder)
        {
            info.title = title.to_owned();
            info.folder = folder.map(Path::to_path_buf);
            self.cache = None;
        }
    }

    /// Every open Forge window, this one included, oldest first.
    pub fn list(&mut self) -> Vec<WindowInfo> {
        if let Some((at, list)) = &self.cache
            && at.elapsed() < LIST_CACHE
        {
            return list.clone();
        }
        let mut list = self.dir.as_deref().map(read_windows).unwrap_or_default();
        // This window's own entry may not be written yet, or may be behind.
        let me = self.info.lock().map(|i| i.clone()).ok();
        if let Some(me) = me {
            match list.iter_mut().find(|w| w.pid == me.pid) {
                Some(entry) => *entry = me,
                None => {
                    list.push(me);
                    list.sort_by_key(|w| (w.started, w.pid));
                }
            }
        }
        self.cache = Some((Instant::now(), list.clone()));
        list
    }

    /// Another window with `folder` open, if there is one.
    pub fn other_with_folder(&mut self, folder: &Path) -> Option<WindowInfo> {
        let me = self.pid();
        self.list()
            .into_iter()
            .find(|w| w.pid != me && w.folder.as_deref() == Some(folder))
    }

    /// Brings the window `pid` to the front.
    pub fn focus(&self, pid: u32) -> anyhow::Result<()> {
        let dir = self
            .dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("no config directory"))?;
        std::fs::write(focus_path(dir, pid), b"")?;
        // The window raises itself when it sees the request, but most systems
        // only let the app in front hand focus to another one.
        platform::allow_focus(pid);
        Ok(())
    }

    /// Raises this window if another one asked for it. Call every frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.focus_requested.swap(false, Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            let delay = Duration::from_millis(300);
            self.refocus_at = Some(Instant::now() + delay);
            ctx.request_repaint_after(delay);
        }
        if let Some(at) = self.refocus_at {
            if Instant::now() >= at {
                self.refocus_at = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else {
                ctx.request_repaint_after(at - Instant::now());
            }
        }
    }

    /// Removes this window from the registry. Called on exit.
    pub fn unregister(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(dir) = &self.dir {
            let pid = self.pid();
            let _ = std::fs::remove_file(entry_path(dir, pid));
            let _ = std::fs::remove_file(focus_path(dir, pid));
        }
    }
}

impl Drop for Windows {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Starts a new, empty Forge window.
pub fn spawn_empty() -> anyhow::Result<()> {
    spawn(&[NEW_WINDOW_FLAG.as_ref()])
}

/// Starts a new Forge window with `folder` open.
pub fn spawn_with_folder(folder: &Path) -> anyhow::Result<()> {
    spawn(&[folder.as_os_str()])
}

fn spawn(args: &[&std::ffi::OsStr]) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe).args(args).spawn()?;
    Ok(())
}

/// Command-line flag for a window that starts without a folder, instead of
/// reopening the last one.
pub const NEW_WINDOW_FLAG: &str = "--new-window";

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};

    /// Activates the other process; only the active app may do this.
    pub fn allow_focus(pid: u32) {
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
            app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        }
    }
}

#[cfg(windows)]
mod platform {
    /// Lets the other process take the foreground when it asks for it.
    pub fn allow_focus(pid: u32) {
        // SAFETY: plain Win32 call with no pointers.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    /// The window manager decides; the other window asks for focus itself.
    pub fn allow_focus(_pid: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-windows-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn info(pid: u32, started: u128, folder: Option<&str>) -> WindowInfo {
        WindowInfo {
            pid,
            title: format!("window {pid}"),
            folder: folder.map(PathBuf::from),
            started,
        }
    }

    #[test]
    fn lists_registered_windows_oldest_first() {
        let dir = temp_dir("list");
        write_entry(&dir, &info(30, 2, Some("/b"))).unwrap();
        write_entry(&dir, &info(10, 1, Some("/a"))).unwrap();
        std::fs::write(dir.join("junk.json"), "not json").unwrap();
        std::fs::write(dir.join("readme.txt"), "ignored").unwrap();
        let pids: Vec<u32> = read_windows(&dir).iter().map(|w| w.pid).collect();
        assert_eq!(pids, [10, 30]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_entries_are_dropped() {
        let dir = temp_dir("stale");
        write_entry(&dir, &info(7, 1, None)).unwrap();
        let path = entry_path(&dir, 7);
        let old = SystemTime::now() - STALE - Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(read_windows(&dir).is_empty());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn window_names() {
        assert_eq!(info(1, 0, Some("/code/forge-ide")).name(), "forge-ide");
        assert_eq!(info(1, 0, None).name(), "Empty Window");
    }

    #[test]
    fn registers_lists_itself_and_unregisters() {
        let dir = temp_dir("register");
        let ctx = egui::Context::default();
        let mut windows = Windows::register_in(Some(dir.clone()), &ctx);
        windows.set("proj — Forge IDE", Some(Path::new("/code/proj")));
        let me = windows.list();
        assert_eq!(me.len(), 1);
        assert_eq!(me[0].folder.as_deref(), Some(Path::new("/code/proj")));
        assert!(windows.other_with_folder(Path::new("/code/proj")).is_none());

        // A focus request is picked up by the heartbeat thread.
        windows.focus(windows.pid()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !windows.focus_requested.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(windows.focus_requested.load(Ordering::SeqCst));

        windows.unregister();
        assert!(!entry_path(&dir, windows.pid()).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

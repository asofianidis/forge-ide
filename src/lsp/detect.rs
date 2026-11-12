//! Locating executables on disk, so we can tell which servers (and which
//! package managers) are already installed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::catalog::ServerSpec;

/// Finds `name` on `PATH` or in well-known per-user tool directories that are
/// often missing from `PATH` (e.g. `~/go/bin` right after installing gopls).
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let dirs = path_dirs.into_iter().chain(extra_search_dirs());
    find_in_dirs(name, dirs, &executable_extensions())
}

/// Returns the server's executable if it is installed and (when the catalog
/// asks for it) responds to its verification command.
pub fn detect_server(spec: &ServerSpec) -> Option<PathBuf> {
    let path = find_executable(spec.command)?;
    match spec.verify_args {
        Some(args) => {
            let mut cmd = Command::new(&path);
            cmd.args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            hide_console_window(&mut cmd);
            cmd.status().ok()?.success().then_some(path)
        }
        None => Some(path),
    }
}

fn find_in_dirs(
    name: &str,
    dirs: impl IntoIterator<Item = PathBuf>,
    extensions: &[OsString],
) -> Option<PathBuf> {
    dirs.into_iter().find_map(|dir| {
        extensions.iter().find_map(|ext| {
            let mut file = OsString::from(name);
            file.push(ext);
            let candidate = dir.join(file);
            is_executable(&candidate).then_some(candidate)
        })
    })
}

/// Suffixes to try after the bare name. On Windows this follows `PATHEXT`, so
/// npm shims like `pyright-langserver.cmd` are found.
fn executable_extensions() -> Vec<OsString> {
    if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        pathext
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| OsString::from(e.to_lowercase()))
            .collect()
    } else {
        vec![OsString::new()]
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn extra_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        for sub in [
            ".cargo/bin",
            "go/bin",
            ".local/bin",
            ".dotnet/tools",
            ".ghcup/bin",
        ] {
            dirs.push(home.join(sub));
        }
    }
    if let Some(gopath) = std::env::var_os("GOPATH") {
        dirs.extend(std::env::split_paths(&gopath).map(|p| p.join("bin")));
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
        dirs.push(PathBuf::from(r"C:\Program Files\LLVM\bin"));
    } else {
        // Homebrew's LLVM is keg-only, so its clangd is not linked onto PATH.
        dirs.push(PathBuf::from("/opt/homebrew/opt/llvm/bin"));
        dirs.push(PathBuf::from("/usr/local/opt/llvm/bin"));
    }
    dirs
}

/// Stops Windows from flashing a console window for each child process.
pub fn hide_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_executable_in_given_dirs() {
        let dir = std::env::temp_dir().join(format!("forge-detect-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exts = executable_extensions();
        let file = {
            let mut f = OsString::from("forge-fake-server");
            f.push(&exts[0]);
            dir.join(f)
        };
        std::fs::write(&file, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let found = find_in_dirs("forge-fake-server", [dir.clone()], &exts);
        let missing = find_in_dirs("forge-no-such-server", [dir.clone()], &exts);
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(found, Some(file));
        assert_eq!(missing, None);
    }
}

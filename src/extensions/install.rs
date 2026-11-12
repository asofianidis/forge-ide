//! Installing extensions from `.zip` files and removing them.
//!
//! A package is a zip with `extension.toml` at its root, or inside a single
//! top-level folder (what you get from zipping the extension's folder).
//! Installing first extracts into a hidden staging folder, checks the result
//! loads, and only then swaps it in, so a bad package never leaves a
//! half-installed extension behind.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use zip::ZipArchive;

use super::manifest::{MANIFEST_FILE, Manifest};

/// Limits that stop a malformed or malicious zip from filling the disk.
const MAX_FILES: usize = 10_000;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// A zip that passed inspection, ready to install.
#[derive(Debug, Clone)]
pub struct Package {
    pub zip: PathBuf,
    pub manifest: Manifest,
    /// Folder inside the zip holding the manifest ("" for the root).
    prefix: PathBuf,
    pub files: usize,
    pub bytes: u64,
}

fn open(zip: &Path) -> Result<ZipArchive<BufReader<File>>, String> {
    let file = File::open(zip).map_err(|e| format!("could not open {}: {e}", zip.display()))?;
    ZipArchive::new(BufReader::new(file)).map_err(|e| format!("not a valid zip file: {e}"))
}

/// Junk that archivers add and that shouldn't be installed.
fn is_junk(path: &Path) -> bool {
    path.components().any(|c| {
        let c = c.as_os_str();
        c == "__MACOSX" || c == ".DS_Store" || c == "Thumbs.db"
    })
}

/// Reads the manifest from a zip and checks the package is installable.
pub fn inspect(zip: &Path) -> Result<Package, String> {
    let mut archive = open(zip)?;
    if archive.len() > MAX_FILES {
        return Err(format!("too many files (more than {MAX_FILES})"));
    }
    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| format!("corrupt zip: {e}"))?;
        let Some(path) = file.enclosed_name() else {
            return Err("the zip contains a path outside its own folder".to_owned());
        };
        if is_junk(&path) {
            continue;
        }
        entries.push((i, path, file.is_dir(), file.size()));
    }

    // The manifest at the root wins; otherwise one folder down.
    let manifest_at = |depth: usize| -> Vec<&(usize, PathBuf, bool, u64)> {
        entries
            .iter()
            .filter(|(_, p, dir, _)| {
                !dir && p.file_name().is_some_and(|n| n == MANIFEST_FILE)
                    && p.components().count() == depth + 1
            })
            .collect()
    };
    let (index, prefix) = match (manifest_at(0).as_slice(), manifest_at(1).as_slice()) {
        ([(i, _, _, _)], _) => (*i, PathBuf::new()),
        ([], [(i, p, _, _)]) => (*i, p.parent().unwrap_or(Path::new("")).to_path_buf()),
        ([], []) => {
            return Err(format!(
                "no {MANIFEST_FILE} found at the top of the zip (or in its only folder)"
            ));
        }
        _ => return Err(format!("the zip contains more than one {MANIFEST_FILE}")),
    };

    let mut src = String::new();
    archive
        .by_index(index)
        .map_err(|e| format!("corrupt zip: {e}"))?
        .take(1024 * 1024)
        .read_to_string(&mut src)
        .map_err(|e| format!("could not read {MANIFEST_FILE}: {e}"))?;
    let manifest = Manifest::parse(&src).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;

    let inside: Vec<_> = entries
        .iter()
        .filter(|(_, p, _, _)| p.starts_with(&prefix))
        .collect();
    for theme in &manifest.themes {
        let path = prefix.join(theme);
        if !inside.iter().any(|(_, p, dir, _)| !dir && *p == path) {
            return Err(format!("theme file {theme} is not in the zip"));
        }
    }
    let bytes: u64 = inside.iter().map(|(_, _, _, size)| size).sum();
    if bytes > MAX_TOTAL_BYTES {
        return Err(format!(
            "the extension is too large ({} MB unpacked)",
            bytes / 1024 / 1024
        ));
    }
    Ok(Package {
        zip: zip.to_path_buf(),
        manifest,
        prefix,
        files: inside.iter().filter(|(_, _, dir, _)| !dir).count(),
        bytes,
    })
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}-{nanos}", std::process::id())
}

/// Installs (or replaces) the extension in `extensions_dir/<id>`. Returns the
/// installed folder.
pub fn install(package: &Package, extensions_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(extensions_dir)
        .map_err(|e| format!("could not create {}: {e}", extensions_dir.display()))?;
    let id = &package.manifest.id;
    let staging = extensions_dir.join(format!(".install-{id}-{}", unique_suffix()));
    let result = extract(package, &staging).and_then(|()| swap_in(&staging, extensions_dir, id));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn extract(package: &Package, dest: &Path) -> Result<(), String> {
    let mut archive = open(&package.zip)?;
    let mut written: u64 = 0;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("corrupt zip: {e}"))?;
        let Some(path) = file.enclosed_name() else {
            return Err("the zip contains a path outside its own folder".to_owned());
        };
        if is_junk(&path) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(&package.prefix) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut target = File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        // Sizes in the zip header can lie; cap what is actually written.
        let remaining = MAX_TOTAL_BYTES.saturating_sub(written);
        let n = std::io::copy(&mut (&mut file).take(remaining + 1), &mut target)
            .map_err(|e| format!("could not extract {}: {e}", rel.display()))?;
        written += n;
        if written > MAX_TOTAL_BYTES {
            return Err("the extension is too large".to_owned());
        }
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            // Keep the executable bits for scripts; nothing setuid or world-writable.
            let mode = (mode & 0o755) | 0o600;
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
        }
    }
    // The extracted copy must load, not just the manifest we inspected.
    let src = std::fs::read_to_string(dest.join(MANIFEST_FILE))
        .map_err(|e| format!("could not read the extracted {MANIFEST_FILE}: {e}"))?;
    let manifest = Manifest::parse(&src)?;
    if manifest != package.manifest {
        return Err("the zip changed while it was being installed".to_owned());
    }
    Ok(())
}

/// Moves `staging` to `dir/id`, replacing what was there. The old version is
/// moved aside first and put back if the move fails.
fn swap_in(staging: &Path, dir: &Path, id: &str) -> Result<PathBuf, String> {
    let target = dir.join(id);
    let backup = dir.join(format!(".old-{id}-{}", unique_suffix()));
    let had_old = target.exists();
    if had_old {
        std::fs::rename(&target, &backup)
            .map_err(|e| format!("could not replace the installed version: {e}"))?;
    }
    if let Err(e) = std::fs::rename(staging, &target) {
        if had_old {
            let _ = std::fs::rename(&backup, &target);
        }
        return Err(format!("could not install into {}: {e}", target.display()));
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(target)
}

/// Deletes an installed extension's folder. Only folders directly inside
/// `extensions_dir` are removed.
pub fn uninstall(dir: &Path, extensions_dir: &Path) -> Result<(), String> {
    if dir.parent() != Some(extensions_dir) {
        return Err(format!("{} is not an installed extension", dir.display()));
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("could not remove {}: {e}", dir.display()))
}

/// Removes staging and backup folders left behind by an interrupted install.
pub fn clean_up(extensions_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(extensions_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".install-") || name.starts_with(".old-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;

    const MANIFEST: &str = "id = \"demo\"\nname = \"Demo\"\nversion = \"1.0.0\"\n";

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("forge-install-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_zip(path: &Path, files: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, content) in files {
            let opts = SimpleFileOptions::default().unix_permissions(0o755);
            zip.start_file(*name, opts).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn installs_from_root_or_single_folder_and_upgrades() {
        let root = temp_dir("install");
        let exts = root.join("extensions");

        let flat = root.join("flat.zip");
        make_zip(&flat, &[(MANIFEST_FILE, MANIFEST), ("run.sh", "echo hi")]);
        let pkg = inspect(&flat).unwrap();
        assert_eq!(pkg.manifest.id, "demo");
        assert_eq!(pkg.files, 2);
        let dir = install(&pkg, &exts).unwrap();
        assert_eq!(dir, exts.join("demo"));
        assert!(dir.join("run.sh").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join("run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "scripts stay executable");
        }

        let nested = root.join("nested.zip");
        let v2 = MANIFEST.replace("1.0.0", "2.0.0");
        make_zip(
            &nested,
            &[
                ("demo/extension.toml", v2.as_str()),
                ("demo/scripts/a.py", "print(1)"),
                ("__MACOSX/demo/._a.py", "junk"),
            ],
        );
        let pkg = inspect(&nested).unwrap();
        assert_eq!(pkg.manifest.version, "2.0.0");
        install(&pkg, &exts).unwrap();
        assert!(dir.join("scripts/a.py").is_file());
        assert!(
            !dir.join("run.sh").exists(),
            "upgrade replaces the old files"
        );
        assert!(!exts.join("__MACOSX").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&exts).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no staging or backup folders left");

        uninstall(&dir, &exts).unwrap();
        assert!(!dir.exists());
        assert!(uninstall(&root, &exts).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_bad_packages() {
        let root = temp_dir("reject");
        let zip = root.join("x.zip");

        make_zip(&zip, &[("readme.md", "no manifest")]);
        assert!(inspect(&zip).unwrap_err().contains("no extension.toml"));

        make_zip(
            &zip,
            &[
                ("a/extension.toml", MANIFEST),
                ("b/extension.toml", MANIFEST),
            ],
        );
        assert!(inspect(&zip).unwrap_err().contains("more than one"));

        make_zip(&zip, &[(MANIFEST_FILE, "id = \"BAD\"")]);
        assert!(inspect(&zip).is_err());

        let with_theme = format!("{MANIFEST}themes = [\"t.toml\"]\n");
        make_zip(&zip, &[(MANIFEST_FILE, with_theme.as_str())]);
        assert!(inspect(&zip).unwrap_err().contains("t.toml"));

        make_zip(
            &zip,
            &[(MANIFEST_FILE, MANIFEST), ("../evil.sh", "rm -rf /")],
        );
        assert!(inspect(&zip).unwrap_err().contains("outside"));

        std::fs::write(&zip, "not a zip").unwrap();
        assert!(inspect(&zip).unwrap_err().contains("not a valid zip"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

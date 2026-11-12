//! Updating a macOS install that runs from `Forge IDE.app`.
//!
//! The release's `.tar.gz` holds the bare `forge-ide` executable, signed on
//! its own. Dropping it into an app bundle would break the bundle's code
//! signature, so a bundle is updated as a whole from the release's disk
//! image: download and verify the `.dmg`, mount it, copy the app out next to
//! the installed one, and swap the two.

// Only the macOS build installs from a disk image; the helpers below are
// still compiled (and tested) everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// The app's folder name, in the disk image and once installed.
pub const APP_NAME: &str = "Forge IDE.app";

/// The `.app` bundle that `exe` runs from, if it sits at
/// `<bundle>.app/Contents/MacOS/<exe>`.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

/// The bundle this process runs from, if any.
pub fn current_bundle() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    bundle_of(&std::env::current_exe().ok()?)
}

/// The disk image `.github/workflows/release.yml` publishes for `version`
/// on this CPU.
pub fn dmg_name(version: &str) -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    format!("Forge-IDE-v{version}-macos-{arch}.dmg")
}

/// Checks the file at `path` against its SHA-256 entry in a `SHA256SUMS` file.
pub fn verify_sha256(path: &Path, sums: &str, file_name: &str) -> Result<()> {
    let expected = match self_update::Checksum::from_sums_file(sums, file_name)? {
        self_update::Checksum::Sha256(hex) => hex.to_ascii_lowercase(),
        _ => bail!("no SHA-256 checksum for {file_name}"),
    };
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        bail!("checksum mismatch for {file_name}");
    }
    Ok(())
}

/// Replaces `bundle` with the app inside the (already verified) disk image
/// at `dmg`, using `work` as scratch space. The running process keeps its
/// open files, so it carries on until it is restarted.
#[cfg(target_os = "macos")]
pub fn install_from_dmg(dmg: &Path, bundle: &Path, work: &Path) -> Result<()> {
    use std::process::Command;

    fn run(cmd: &mut Command) -> Result<()> {
        let output = cmd.output().with_context(|| format!("running {cmd:?}"))?;
        if !output.status.success() {
            bail!(
                "{cmd:?} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    let parent = bundle
        .parent()
        .context("the app bundle has no parent folder")?;
    // Staged next to the installed app, so the swap is a rename on one volume.
    let staged = parent.join(format!(".{APP_NAME}.update"));
    let old = parent.join(format!(".{APP_NAME}.old"));
    for leftover in [&staged, &old] {
        if leftover.exists() {
            std::fs::remove_dir_all(leftover)
                .with_context(|| format!("removing {}", leftover.display()))?;
        }
    }

    let mount = work.join("mnt");
    std::fs::create_dir_all(&mount)?;
    run(Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-noautoopen",
            "-readonly",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(dmg))?;
    let copied = run(Command::new("ditto").arg(mount.join(APP_NAME)).arg(&staged));
    let detached = run(Command::new("hdiutil")
        .args(["detach", "-force"])
        .arg(&mount));
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e.context("copying the new app out of the disk image"));
    }
    if let Err(e) = detached {
        eprintln!("forge-ide: could not unmount the update's disk image: {e:#}");
    }

    std::fs::rename(bundle, &old).with_context(|| format!("moving {} aside", bundle.display()))?;
    if let Err(e) = std::fs::rename(&staged, bundle) {
        // Put the old app back so the install isn't left without one.
        let _ = std::fs::rename(&old, bundle);
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e).with_context(|| format!("installing the new {APP_NAME}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_bundle_around_the_executable() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Forge IDE.app/Contents/MacOS/forge-ide"
            )),
            Some(PathBuf::from("/Applications/Forge IDE.app"))
        );
        assert_eq!(bundle_of(Path::new("/usr/local/bin/forge-ide")), None);
        assert_eq!(
            bundle_of(Path::new("/tmp/Forge IDE/Contents/MacOS/forge-ide")),
            None
        );
    }

    #[test]
    fn dmg_name_avoids_the_target_triple() {
        // The bare-binary updater picks assets by target triple, so the disk
        // image must not look like one of its archives.
        let name = dmg_name("0.1.42");
        assert!(name.starts_with("Forge-IDE-v0.1.42-macos-"), "{name}");
        assert!(name.ends_with(".dmg"));
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            assert!(!name.contains(triple));
        }
        assert!(!name.contains("darwin"));
    }

    #[test]
    fn verifies_against_sha256sums() {
        let dir = std::env::temp_dir().join(format!("forge-mac-app-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.dmg");
        std::fs::write(&file, b"hello").unwrap();
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  a.dmg\n";
        let bad = "0000000000000000000000000000000000000000000000000000000000000000  a.dmg\n";
        assert!(verify_sha256(&file, good, "a.dmg").is_ok());
        assert!(verify_sha256(&file, bad, "a.dmg").is_err());
        assert!(verify_sha256(&file, good, "b.dmg").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! Self-updating from GitHub releases.
//!
//! Release builds (made by `.github/workflows/release.yml`) have their
//! version baked in through the `FORGE_VERSION` environment variable and
//! check for a newer release on startup. Local `cargo build`s report the
//! `Cargo.toml` version and only check when asked (Help → Check for Updates).
//!
//! Checks and downloads run on a background thread; the UI polls
//! [`Updater::state`] each frame.
//!
//! A plain install is updated by swapping in the release's `.tar.gz`
//! executable. On macOS, a `Forge IDE.app` install is replaced as a whole
//! from the release's disk image instead (see [`mac_app`]).

mod mac_app;

use std::sync::{Arc, Mutex};

use self_update::backends::github;

const REPO_OWNER: &str = "asofianidis";
const REPO_NAME: &str = "forge-ide";
const BIN_NAME: &str = "forge-ide";
/// Each release publishes this checksum file; downloads are verified against it.
const CHECKSUMS_ASSET: &str = "SHA256SUMS";

/// The running version: the release version for CI builds, else `Cargo.toml`'s.
pub const VERSION: &str = match option_env!("FORGE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Whether this binary was built by the release workflow.
pub const IS_RELEASE_BUILD: bool = option_env!("FORGE_VERSION").is_some();

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    /// A newer version is published and can be installed.
    Available(String),
    /// Download progress in `0.0..=1.0`, if the size is known.
    Installing(Option<f32>),
    /// The new version is on disk; it takes effect after a restart.
    Installed(String),
    Failed(String),
}

#[derive(Clone)]
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(UpdateState::Idle)),
        }
    }
}

impl Updater {
    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }

    fn set(&self, ctx: &egui::Context, state: UpdateState) {
        *self.state.lock().unwrap() = state;
        ctx.request_repaint();
    }

    fn is_busy(&self) -> bool {
        matches!(
            self.state(),
            UpdateState::Checking | UpdateState::Installing(_)
        )
    }

    /// Looks for a newer release in the background.
    pub fn check(&self, ctx: &egui::Context) {
        if self.is_busy() {
            return;
        }
        self.set(ctx, UpdateState::Checking);
        let this = self.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let state = match newer_version() {
                Ok(Some(version)) => UpdateState::Available(version),
                Ok(None) => UpdateState::UpToDate,
                Err(e) => UpdateState::Failed(format!("Update check failed: {e}")),
            };
            this.set(&ctx, state);
        });
    }

    /// Downloads and installs the newest release in the background,
    /// replacing the running executable.
    pub fn install(&self, ctx: &egui::Context) {
        if self.is_busy() {
            return;
        }
        self.set(ctx, UpdateState::Installing(None));
        let this = self.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let progress = {
                let this = this.clone();
                let ctx = ctx.clone();
                move |done: u64, total: Option<u64>| {
                    let fraction = total
                        .filter(|&t| t > 0)
                        .map(|t| (done as f32 / t as f32).min(1.0));
                    this.set(&ctx, UpdateState::Installing(fraction));
                }
            };
            let result = match mac_app::current_bundle() {
                #[cfg(target_os = "macos")]
                Some(bundle) => install_app_bundle(&bundle, progress),
                _ => install_executable(progress),
            };
            let state = match result {
                Ok(Some(version)) => UpdateState::Installed(version),
                Ok(None) => UpdateState::UpToDate,
                Err(e) => UpdateState::Failed(format!("Update failed: {e:#}")),
            };
            this.set(&ctx, state);
        });
    }
}

fn updater_builder() -> github::UpdateBuilder {
    let mut builder = github::Update::configure();
    builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(VERSION)
        .checksum_from_asset(CHECKSUMS_ASSET)
        .show_output(false)
        .show_download_progress(false)
        .no_confirm(true);
    // Debug builds can point at a mock GitHub API to test the update flow.
    if cfg!(debug_assertions)
        && let Ok(url) = std::env::var("FORGE_UPDATE_API_URL")
    {
        builder.api_base_url(url);
    }
    builder
}

/// Replaces the running executable with the newest release's. Returns the
/// installed version, or `None` if already up to date.
fn install_executable(
    progress: impl Fn(u64, Option<u64>) + Send + Sync + 'static,
) -> anyhow::Result<Option<String>> {
    let status = updater_builder()
        .progress_callback(progress)
        .build()
        .and_then(|u| u.update())?;
    Ok(status.is_updated().then(|| status.version().to_owned()))
}

/// Replaces the `Forge IDE.app` this process runs from with the newest
/// release's, from its disk image. Returns the installed version, or `None`
/// if already up to date.
#[cfg(target_os = "macos")]
fn install_app_bundle(
    bundle: &std::path::Path,
    progress: impl Fn(u64, Option<u64>) + Send + Sync + 'static,
) -> anyhow::Result<Option<String>> {
    use anyhow::Context;

    let Some(release) = newest_release()? else {
        return Ok(None);
    };
    let version = release.version().to_owned();
    let dmg_name = mac_app::dmg_name(&version);
    let asset = |name: &str| {
        release
            .assets()
            .iter()
            .find(|a| a.name() == name)
            .cloned()
            .with_context(|| format!("release {version} has no {name}"))
    };
    let dmg_asset = asset(&dmg_name)?;
    let sums_asset = asset(CHECKSUMS_ASSET)?;

    let work = std::env::temp_dir().join(format!("forge-ide-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let result = (|| -> anyhow::Result<()> {
        let mut sums = Vec::new();
        download(sums_asset.download_url()).download_to(&mut sums)?;
        let dmg = work.join(&dmg_name);
        download(dmg_asset.download_url())
            .progress_callback(progress)
            .download_to(std::fs::File::create(&dmg)?)?;
        mac_app::verify_sha256(&dmg, &String::from_utf8_lossy(&sums), &dmg_name)?;
        mac_app::install_from_dmg(&dmg, bundle, &work)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result.map(|()| Some(version))
}

/// A download of a GitHub release asset through its API URL.
#[cfg(target_os = "macos")]
fn download(url: &str) -> self_update::Download {
    let mut download = self_update::Download::from_url(url);
    download.request_header(
        self_update::http::header::ACCEPT,
        "application/octet-stream",
    );
    download
}

/// Whether `release` has something this install can update from: the disk
/// image for an app bundle, else the archive for this platform.
fn has_install_asset(release: &self_update::update::Release, in_bundle: bool) -> bool {
    if in_bundle {
        let dmg = mac_app::dmg_name(release.version());
        release.assets().iter().any(|a| a.name() == dmg)
    } else {
        release.has_target_asset(self_update::get_target())
    }
}

/// The newest published release this install can update to, if it is newer
/// than [`VERSION`].
fn newest_release() -> self_update::Result<Option<self_update::update::Release>> {
    let updater = updater_builder().build()?;
    let releases = updater.get_newer_releases()?;
    let in_bundle = mac_app::current_bundle().is_some();
    Ok(releases
        .all()
        .iter()
        .filter(|r| has_install_asset(r, in_bundle))
        .max_by(|a, b| compare_versions(a.version(), b.version()))
        .cloned())
}

/// The newest published version this install can update to, if it is newer
/// than [`VERSION`].
fn newer_version() -> self_update::Result<Option<String>> {
    Ok(newest_release()?.map(|r| r.version().to_owned()))
}

/// Compares dotted numeric versions (`1.2.10` > `1.2.9`). Non-numeric
/// parts compare as 0.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parts(a).cmp(&parts(b))
}

/// Relaunches the (updated) executable and exits this process.
pub fn restart() -> anyhow::Result<()> {
    self_update::restart::restart()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("0.1.10", "0.1.9"), Ordering::Greater);
        assert_eq!(compare_versions("v0.2.0", "0.1.99"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.3", "0.1.3"), Ordering::Equal);
    }

    #[test]
    fn local_builds_use_cargo_version() {
        if !IS_RELEASE_BUILD {
            assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        }
    }
}

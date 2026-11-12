//! Extensions: folders with an `extension.toml` manifest that add commands
//! (external programs run with editor context) and color themes.
//!
//! Extensions live in `<config dir>/extensions/<id>/` and are installed from
//! `.zip` files ([`install`]). They run as separate processes ([`run`]), so a
//! misbehaving extension can't crash or block the editor. The authoring guide
//! is `docs/extensions.md`.

pub mod install;
pub mod manifest;
pub mod panel;
pub mod run;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::KeyboardShortcut;

pub use manifest::{CommandSpec, Input, Manifest, Output};
pub use panel::{ExtensionsPanel, PanelAction};

use crate::commands::Command;
use manifest::MANIFEST_FILE;

/// The authoring guide, shown from the Extensions view.
pub const GUIDE: &str = include_str!("../../docs/extensions.md");

/// Directory extensions are installed into.
pub fn extensions_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FORGE_EXTENSIONS_DIR") {
        return Some(PathBuf::from(dir));
    }
    crate::theme::config_dir().map(|d| d.join("extensions"))
}

/// An installed extension, loaded or not.
#[derive(Debug, Clone)]
pub struct Extension {
    pub dir: PathBuf,
    /// `None` when the manifest is missing or invalid; see `error`.
    pub manifest: Option<Manifest>,
    pub error: Option<String>,
    /// Problems that don't stop the extension from loading (e.g. a
    /// keybinding that clashes with a built-in one).
    pub warnings: Vec<String>,
    pub enabled: bool,
    /// Names of the contributed themes, read from their files.
    pub theme_names: Vec<String>,
}

impl Extension {
    /// The manifest id, or the folder name for a broken extension.
    pub fn id(&self) -> String {
        match &self.manifest {
            Some(m) => m.id.clone(),
            None => folder_name(&self.dir),
        }
    }

    pub fn name(&self) -> String {
        match &self.manifest {
            Some(m) => m.name.clone(),
            None => folder_name(&self.dir),
        }
    }

    /// Loaded and switched on.
    pub fn is_active(&self) -> bool {
        self.enabled && self.manifest.is_some()
    }

    pub fn readme(&self) -> Option<PathBuf> {
        ["README.md", "readme.md", "Readme.md"]
            .iter()
            .map(|n| self.dir.join(n))
            .find(|p| p.is_file())
    }

    pub fn commands(&self) -> &[CommandSpec] {
        self.manifest.as_ref().map_or(&[], |m| &m.commands)
    }

    fn load(dir: PathBuf, disabled: &[String]) -> Self {
        let result = std::fs::read_to_string(dir.join(MANIFEST_FILE))
            .map_err(|e| format!("could not read {MANIFEST_FILE}: {e}"))
            .and_then(|src| Manifest::parse(&src).map_err(|e| format!("{MANIFEST_FILE}: {e}")))
            .and_then(|m| {
                for theme in &m.themes {
                    if !dir.join(theme).is_file() {
                        return Err(format!("theme file {theme} is missing"));
                    }
                }
                Ok(m)
            });
        let (manifest, error) = match result {
            Ok(m) => (Some(m), None),
            Err(e) => (None, Some(e)),
        };
        let theme_names = manifest
            .iter()
            .flat_map(|m| &m.themes)
            .filter_map(|t| {
                let src = std::fs::read_to_string(dir.join(t)).ok()?;
                let table: toml::Table = toml::from_str(&src).ok()?;
                Some(table.get("name")?.as_str()?.to_owned())
            })
            .collect();
        let mut ext = Self {
            dir,
            manifest,
            error,
            warnings: Vec::new(),
            enabled: true,
            theme_names,
        };
        ext.enabled = !disabled.contains(&ext.id());
        ext
    }
}

fn folder_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A command reachable by shortcut.
#[derive(Debug, Clone)]
pub struct Binding {
    pub command: String,
    pub shortcut: KeyboardShortcut,
    pub edits_text: bool,
}

/// All installed extensions.
#[derive(Debug, Default)]
pub struct Extensions {
    pub dir: Option<PathBuf>,
    pub list: Vec<Extension>,
    /// Shortcuts of active extensions' commands, most modifiers first.
    pub bindings: Vec<Binding>,
}

impl Extensions {
    /// Loads every extension in `dir` (sorted by name). Extensions whose id is
    /// in `disabled` are loaded but switched off.
    pub fn load(
        dir: Option<PathBuf>,
        disabled: &[String],
        builtin: &[(Command, KeyboardShortcut)],
    ) -> Self {
        let mut list: Vec<Extension> = dir
            .as_deref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            // Dot folders are install staging areas (and hidden junk).
            .filter(|p| p.is_dir() && !folder_name(p).starts_with('.'))
            .map(|p| Extension::load(p, disabled))
            .collect();
        list.sort_by_key(|e| (e.name().to_lowercase(), e.id()));

        let mut seen = HashSet::new();
        for ext in &mut list {
            if ext.manifest.is_some() && !seen.insert(ext.id()) {
                ext.error = Some(format!(
                    "another extension already has the id {:?}",
                    ext.id()
                ));
                ext.manifest = None;
            }
        }
        let mut exts = Self {
            dir,
            list,
            bindings: Vec::new(),
        };
        exts.bind_shortcuts(builtin);
        exts
    }

    /// Collects shortcuts of active commands, warning about any that clash
    /// with a built-in shortcut (from the keymap in effect) or an earlier
    /// extension. Call again when the keymap changes.
    pub fn bind_shortcuts(&mut self, builtin: &[(Command, KeyboardShortcut)]) {
        let mut bindings: Vec<Binding> = Vec::new();
        for ext in &mut self.list {
            ext.warnings.clear();
            if !ext.is_active() {
                continue;
            }
            let id = ext.id();
            let mut warnings = Vec::new();
            for cmd in ext.commands() {
                let Some(shortcut) = cmd.shortcut() else {
                    continue;
                };
                let label = cmd.keybinding.as_deref().unwrap_or_default();
                if let Some((c, _)) = builtin.iter().find(|(_, s)| same_keys(s, &shortcut)) {
                    warnings.push(format!(
                        "{}: {label} is already used by {}",
                        cmd.title,
                        c.palette_label()
                    ));
                    continue;
                }
                if let Some(b) = bindings.iter().find(|b| same_keys(&b.shortcut, &shortcut)) {
                    warnings.push(format!(
                        "{}: {label} is already used by {}",
                        cmd.title, b.command
                    ));
                    continue;
                }
                bindings.push(Binding {
                    command: qualified_id(&id, &cmd.id),
                    shortcut,
                    edits_text: cmd.output.edits_text(),
                });
            }
            ext.warnings.extend(warnings);
        }
        // egui matches shortcuts logically (Cmd+L also fires for Cmd+Shift+L),
        // so check the ones with more modifiers first.
        bindings.sort_by_key(|b| {
            let m = b.shortcut.modifiers;
            std::cmp::Reverse(
                [m.alt, m.shift, m.ctrl, m.command]
                    .iter()
                    .filter(|&&x| x)
                    .count(),
            )
        });
        self.bindings = bindings;
    }

    pub fn get(&self, id: &str) -> Option<&Extension> {
        self.list.iter().find(|e| e.id() == id)
    }

    /// Commands of active extensions with their qualified ids.
    pub fn commands(&self) -> impl Iterator<Item = (&Extension, &CommandSpec, String)> {
        self.list.iter().filter(|e| e.is_active()).flat_map(|e| {
            let id = e.id();
            e.commands()
                .iter()
                .map(move |c| (e, c, qualified_id(&id, &c.id)))
        })
    }

    /// Looks up an active command by `extension-id:command-id`.
    pub fn command(&self, qualified: &str) -> Option<(&Extension, &CommandSpec)> {
        let (ext, cmd) = qualified.split_once(':')?;
        let ext = self.get(ext).filter(|e| e.is_active())?;
        let cmd = ext.commands().iter().find(|c| c.id == cmd)?;
        Some((ext, cmd))
    }

    /// Theme files contributed by active extensions.
    pub fn theme_files(&self) -> Vec<PathBuf> {
        self.list
            .iter()
            .filter(|e| e.is_active())
            .flat_map(|e| {
                let themes = e.manifest.as_ref().map_or(&[][..], |m| &m.themes);
                themes.iter().map(|t| e.dir.join(t))
            })
            .collect()
    }

    pub fn active_count(&self) -> usize {
        self.list.iter().filter(|e| e.is_active()).count()
    }
}

/// `extension-id:command-id`, how commands are named outside their manifest.
pub fn qualified_id(ext: &str, cmd: &str) -> String {
    format!("{ext}:{cmd}")
}

fn same_keys(a: &KeyboardShortcut, b: &KeyboardShortcut) -> bool {
    a.modifiers == b.modifiers && a.logical_key == b.logical_key
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn write_extension(root: &Path, folder: &str, manifest: &str) -> PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), manifest).unwrap();
        dir
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("forge-ext-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_valid_broken_and_disabled_extensions() {
        let root = temp_dir("load");
        write_extension(
            &root,
            "good",
            r#"
                id = "good"
                name = "Good"
                version = "1.0.0"
                [[commands]]
                id = "hi"
                title = "Say Hi"
                program = "echo"
                keybinding = "Cmd+Alt+J"
                [[commands]]
                id = "clash"
                title = "Clash"
                program = "echo"
                keybinding = "Cmd+S"
            "#,
        );
        write_extension(&root, "broken", "id = 3");
        write_extension(
            &root,
            "off",
            "id = \"off\"\nname = \"Off\"\nversion = \"1\"",
        );
        std::fs::create_dir_all(root.join(".install-staging")).unwrap();

        let keymap = crate::keymap::Keymap::default();
        let exts = Extensions::load(Some(root.clone()), &["off".to_owned()], keymap.table());
        let ids: Vec<String> = exts.list.iter().map(Extension::id).collect();
        assert_eq!(ids, ["broken", "good", "off"]);

        let broken = exts.get("broken").unwrap();
        assert!(broken.error.is_some() && !broken.is_active());
        assert!(!exts.get("off").unwrap().is_active());

        let good = exts.get("good").unwrap();
        assert!(good.is_active());
        assert_eq!(good.warnings.len(), 1, "Cmd+S clashes with Save");
        assert_eq!(exts.bindings.len(), 1);
        assert_eq!(exts.bindings[0].command, "good:hi");
        assert!(exts.command("good:hi").is_some());
        assert!(exts.command("good:nope").is_none());
        assert_eq!(exts.commands().count(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_theme_file_is_an_error() {
        let root = temp_dir("theme");
        write_extension(
            &root,
            "t",
            "id = \"t\"\nname = \"T\"\nversion = \"1\"\nthemes = [\"dusk.toml\"]",
        );
        let exts = Extensions::load(Some(root.clone()), &[], &[]);
        assert!(exts.list[0].error.as_deref().unwrap().contains("dusk.toml"));
        std::fs::write(root.join("t/dusk.toml"), "name = \"Dusk\"").unwrap();
        let exts = Extensions::load(Some(root.clone()), &[], &[]);
        assert_eq!(exts.theme_files(), [root.join("t/dusk.toml")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn example_extension_is_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/extensions/text-tools");
        let ext = Extension::load(dir, &[]);
        assert_eq!(ext.error, None);
        assert!(ext.commands().len() >= 3);
    }
}

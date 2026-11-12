//! The `extension.toml` manifest: who an extension is and what it adds.
//!
//! The format is documented for extension authors in `docs/extensions.md`;
//! keep the two in sync.

use std::collections::HashSet;
use std::path::{Component, Path};

use egui::{Key, KeyboardShortcut, Modifiers};
use serde::Deserialize;

/// File name of the manifest at the root of every extension.
pub const MANIFEST_FILE: &str = "extension.toml";

/// Default time limit for a command, in seconds.
const DEFAULT_TIMEOUT: u64 = 60;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Unique, stable identifier; also the name of the install folder.
    pub id: String,
    /// Display name.
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    /// Oldest Forge version the extension works with, e.g. `"0.1.0"`.
    #[serde(default)]
    pub min_forge_version: Option<String>,
    /// Theme files (relative to the extension folder), same format as custom themes.
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
}

/// A command the extension adds to the command palette and sidebar.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Unique within the extension.
    pub id: String,
    pub title: String,
    /// Program to run: a name looked up on `PATH`, or a path relative to the
    /// extension folder (anything containing a `/`).
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Replaces `program` / `args` on Windows.
    #[serde(default)]
    pub windows: Option<PlatformOverride>,
    /// Replaces `program` / `args` on macOS.
    #[serde(default)]
    pub macos: Option<PlatformOverride>,
    /// Replaces `program` / `args` on Linux.
    #[serde(default)]
    pub linux: Option<PlatformOverride>,
    /// Working directory (variables allowed). Defaults to the open folder,
    /// else the file's folder, else the extension folder.
    #[serde(default)]
    pub cwd: Option<String>,
    /// What to write to the program's standard input.
    #[serde(default)]
    pub stdin: Input,
    /// What to do with the program's standard output.
    #[serde(default)]
    pub output: Output,
    /// Shortcut such as `"Cmd+Shift+L"` (`Cmd` is Ctrl on Windows and Linux).
    #[serde(default)]
    pub keybinding: Option<String>,
    /// File extensions (`"rs"`) or exact names (`"Dockerfile"`) the command
    /// applies to. Empty means every file.
    #[serde(default)]
    pub file_types: Vec<String>,
    /// Run automatically after a matching file is saved.
    #[serde(default)]
    pub on_save: bool,
    /// Ask the user for a line of text first, available as `${input}`.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Seconds before the program is stopped; 0 for no limit.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformOverride {
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Input {
    /// Nothing (stdin is closed).
    #[default]
    None,
    /// The selected text (empty when nothing is selected).
    Selection,
    /// The whole text of the active editor, saved or not.
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Output {
    /// First line of stdout in the status bar.
    #[default]
    Status,
    /// stdout and stderr in an output tab.
    Panel,
    /// stdout replaces the selection (or is inserted at the caret).
    ReplaceSelection,
    /// stdout replaces the whole document (formatters).
    ReplaceDocument,
    /// stdout is inserted at the caret.
    Insert,
    /// stdout opens in a new untitled tab.
    NewDocument,
    /// Nothing is shown unless the command fails.
    Silent,
}

impl Output {
    /// Whether the output edits the active document.
    pub fn edits_text(self) -> bool {
        matches!(
            self,
            Output::ReplaceSelection | Output::ReplaceDocument | Output::Insert
        )
    }
}

/// Variables that can appear as `${name}` in `program`, `args` and `cwd`.
pub const VARIABLES: &[&str] = &[
    "file",
    "fileBasename",
    "fileBasenameNoExtension",
    "fileExtname",
    "fileDirname",
    "relativeFile",
    "workspaceFolder",
    "extensionPath",
    "selectedText",
    "lineNumber",
    "column",
    "input",
];

/// Variables that need the active tab to be a saved file.
pub const FILE_VARIABLES: &[&str] = &[
    "file",
    "fileBasename",
    "fileBasenameNoExtension",
    "fileExtname",
    "fileDirname",
    "relativeFile",
];

impl Manifest {
    pub fn parse(src: &str) -> Result<Self, String> {
        let manifest: Manifest = toml::from_str(src).map_err(|e| match e.span() {
            Some(span) => {
                let line = src[..span.start.min(src.len())].matches('\n').count() + 1;
                format!("line {line}: {}", e.message())
            }
            None => e.message().to_owned(),
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Checks everything that can be checked without the extension's files.
    pub fn validate(&self) -> Result<(), String> {
        check_id(&self.id).map_err(|e| format!("id: {e}"))?;
        if self.name.trim().is_empty() {
            return Err("name must not be empty".to_owned());
        }
        if self.version.trim().is_empty() {
            return Err("version must not be empty".to_owned());
        }
        if let Some(min) = &self.min_forge_version {
            let have = env!("CARGO_PKG_VERSION");
            let (Some(want), Some(have_v)) = (parse_version(min), parse_version(have)) else {
                return Err(format!("min_forge_version: {min:?} is not a version"));
            };
            if want > have_v {
                return Err(format!("needs Forge {min} or newer (this is {have})"));
            }
        }
        for theme in &self.themes {
            check_relative(theme).map_err(|e| format!("themes: {theme:?} {e}"))?;
        }
        let mut ids = HashSet::new();
        for cmd in &self.commands {
            let ctx = |e: String| format!("command {:?}: {e}", cmd.id);
            check_id(&cmd.id).map_err(|e| ctx(format!("id: {e}")))?;
            if !ids.insert(cmd.id.as_str()) {
                return Err(ctx("id is used twice".to_owned()));
            }
            if cmd.title.trim().is_empty() {
                return Err(ctx("title must not be empty".to_owned()));
            }
            let (program, args) = cmd.invocation();
            if program.trim().is_empty() {
                return Err(ctx("program must not be empty".to_owned()));
            }
            for template in std::iter::once(program)
                .chain(args.iter().map(String::as_str))
                .chain(cmd.cwd.as_deref())
            {
                variables(template).map_err(ctx)?;
            }
            if let Some(k) = &cmd.keybinding {
                parse_shortcut(k).map_err(|e| ctx(format!("keybinding: {e}")))?;
            }
            if cmd.on_save {
                if cmd.prompt.is_some() {
                    return Err(ctx("on_save commands can't have a prompt".to_owned()));
                }
                if cmd.stdin == Input::Selection {
                    return Err(ctx("on_save commands can't read the selection".to_owned()));
                }
                if !matches!(
                    cmd.output,
                    Output::Status | Output::Panel | Output::Silent | Output::ReplaceDocument
                ) {
                    return Err(ctx(
                        "on_save commands can only use the status, panel, silent or \
                         replace-document output"
                            .to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The programs its commands run (shown before installing).
    pub fn programs(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for cmd in &self.commands {
            let program = cmd.invocation().0.to_owned();
            if !seen.contains(&program) {
                seen.push(program);
            }
        }
        seen
    }
}

impl CommandSpec {
    /// Program and arguments for the current platform.
    pub fn invocation(&self) -> (&str, &[String]) {
        let os = if cfg!(windows) {
            &self.windows
        } else if cfg!(target_os = "macos") {
            &self.macos
        } else {
            &self.linux
        };
        let program = os
            .as_ref()
            .and_then(|o| o.program.as_deref())
            .unwrap_or(&self.program);
        let args = os
            .as_ref()
            .and_then(|o| o.args.as_deref())
            .unwrap_or(&self.args);
        (program, args)
    }

    /// Whether the command applies to a file named `file_name` (`None` for an
    /// untitled tab, which only matches commands without `file_types`).
    pub fn applies_to(&self, file_name: Option<&str>) -> bool {
        if self.file_types.is_empty() {
            return true;
        }
        let Some(name) = file_name else {
            return false;
        };
        let ext = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        self.file_types.iter().any(|t| {
            let t = t.trim_start_matches('.');
            t == "*" || t.eq_ignore_ascii_case(ext) || t == name
        })
    }

    pub fn shortcut(&self) -> Option<KeyboardShortcut> {
        self.keybinding
            .as_deref()
            .and_then(|k| parse_shortcut(k).ok())
    }
}

/// Extension and command ids: lowercase ASCII letters, digits, `-`, `_` and
/// `.`, starting with a letter or digit.
pub fn check_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if !first_ok || !rest_ok || id.len() > 64 {
        return Err(format!(
            "{id:?} must be 1-64 lowercase letters, digits, '-', '_' or '.', \
             starting with a letter or digit"
        ));
    }
    Ok(())
}

/// A path inside the extension folder: relative, without `..`.
fn check_relative(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if path.is_empty()
        || p.components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("must be a relative path inside the extension".to_owned());
    }
    Ok(())
}

/// `"1.2.3"` → `[1, 2, 3]`; missing parts count as 0 and anything after a
/// `-` or `+` (pre-release, build) is ignored.
pub fn parse_version(v: &str) -> Option<[u64; 3]> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut out = [0; 3];
    for (i, part) in core.split('.').enumerate() {
        *out.get_mut(i)? = part.parse().ok()?;
    }
    Some(out)
}

/// The variable names used in `template`, or an error for an unknown or
/// unterminated `${…}`.
pub fn variables(template: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    expand(template, |name| {
        names.push(name.to_owned());
        Some(String::new())
    })?;
    Ok(names)
}

/// Replaces each `${name}` in `template` with `lookup(name)`. `$$` is a
/// literal `$`. Errors on an unknown variable, or when `lookup` has no value
/// for one.
pub fn expand(
    template: &str,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('$') {
            out.push('$');
            rest = r;
        } else if let Some(r) = rest.strip_prefix('{') {
            let end = r
                .find('}')
                .ok_or_else(|| format!("unterminated ${{ in {template:?}"))?;
            let name = &r[..end];
            if !VARIABLES.contains(&name) {
                return Err(format!("unknown variable ${{{name}}}"));
            }
            let value = lookup(name).ok_or_else(|| match name {
                n if FILE_VARIABLES.contains(&n) => "save the file first".to_owned(),
                "workspaceFolder" => "open a folder first".to_owned(),
                n => format!("${{{n}}} has no value here"),
            })?;
            out.push_str(&value);
            rest = &r[end + 1..];
        } else {
            out.push('$');
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Parses `"Cmd+Shift+L"`-style shortcuts. `Cmd` (or `Mod`) is Command on
/// macOS and Ctrl elsewhere; `Ctrl` is always Ctrl; `Alt` / `Option`; `Shift`.
pub fn parse_shortcut(s: &str) -> Result<KeyboardShortcut, String> {
    let mut modifiers = Modifiers::NONE;
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = match parts.as_slice() {
        // "Cmd++": the key is the plus sign itself.
        [mods @ .., "", ""] if !mods.is_empty() => ("+", mods),
        [mods @ .., key] => (*key, mods),
        [] => return Err("empty".to_owned()),
    };
    for m in mods {
        modifiers = modifiers.plus(match m.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "mod" | "super" => Modifiers::COMMAND,
            "ctrl" | "control" => Modifiers::CTRL,
            "alt" | "option" | "opt" => Modifiers::ALT,
            "shift" => Modifiers::SHIFT,
            other => return Err(format!("unknown modifier {other:?}")),
        });
    }
    let key = Key::from_name(key)
        .or_else(|| {
            // Accept "f5", "space", "enter" in any case.
            let mut c = key.chars();
            let cap: String = c
                .next()
                .map(|f| {
                    f.to_uppercase()
                        .chain(c.flat_map(char::to_lowercase))
                        .collect()
                })
                .unwrap_or_default();
            Key::from_name(&cap).or_else(|| Key::from_name(&key.to_uppercase()))
        })
        .ok_or_else(|| format!("unknown key {key:?}"))?;
    let is_function_key = key.name().starts_with('F') && key.name().len() > 1;
    if modifiers.is_none() && !is_function_key {
        return Err("needs a modifier (Cmd, Ctrl or Alt) unless it is a function key".to_owned());
    }
    if modifiers == Modifiers::SHIFT && !is_function_key {
        return Err("Shift alone would block typing; add Cmd, Ctrl or Alt".to_owned());
    }
    Ok(KeyboardShortcut::new(modifiers, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        id = "hello"
        name = "Hello"
        version = "1.0.0"
    "#;

    fn with_command(cmd: &str) -> Result<Manifest, String> {
        Manifest::parse(&format!("{MINIMAL}\n[[commands]]\n{cmd}"))
    }

    #[test]
    fn minimal_manifest_parses() {
        let m = Manifest::parse(MINIMAL).unwrap();
        assert_eq!(m.id, "hello");
        assert!(m.commands.is_empty());
    }

    #[test]
    fn command_defaults() {
        let m = with_command("id = \"a\"\ntitle = \"A\"\nprogram = \"echo\"").unwrap();
        let c = &m.commands[0];
        assert_eq!(c.stdin, Input::None);
        assert_eq!(c.output, Output::Status);
        assert_eq!(c.timeout, DEFAULT_TIMEOUT);
        assert!(!c.on_save);
    }

    #[test]
    fn rejects_bad_manifests() {
        assert!(Manifest::parse("id = \"Hello\"\nname = \"x\"\nversion = \"1\"").is_err());
        assert!(Manifest::parse("id = \"x\"\nname = \"\"\nversion = \"1\"").is_err());
        assert!(Manifest::parse(&format!("{MINIMAL}\ntypo = 1")).is_err());
        assert!(Manifest::parse(&format!("{MINIMAL}\nthemes = [\"../x.toml\"]")).is_err());
        assert!(Manifest::parse(&format!("{MINIMAL}\nmin_forge_version = \"999.0\"")).is_err());
        let cmd = "id = \"a\"\ntitle = \"A\"\nprogram = \"echo\"\n";
        assert!(with_command(&format!("{cmd}args = [\"${{nope}}\"]")).is_err());
        assert!(with_command(&format!("{cmd}keybinding = \"Cmd+Nope\"")).is_err());
        assert!(with_command(&format!("{cmd}on_save = true\noutput = \"insert\"")).is_err());
        assert!(with_command(&format!("{cmd}on_save = true\nprompt = \"?\"")).is_err());
        let twice = format!("{MINIMAL}\n[[commands]]\n{cmd}\n[[commands]]\n{cmd}");
        assert!(Manifest::parse(&twice).is_err());
    }

    #[test]
    fn expands_variables() {
        let lookup = |n: &str| match n {
            "file" => Some("/a/b.rs".to_owned()),
            "input" => Some("x y".to_owned()),
            _ => None,
        };
        assert_eq!(expand("${file}:${input}", lookup).unwrap(), "/a/b.rs:x y");
        assert_eq!(
            expand("$$HOME and $PATH", lookup).unwrap(),
            "$HOME and $PATH"
        );
        assert_eq!(
            expand("${fileDirname}", lookup).unwrap_err(),
            "save the file first"
        );
        assert!(expand("${file", lookup).is_err());
        assert_eq!(variables("a ${file} ${input}").unwrap(), ["file", "input"]);
    }

    #[test]
    fn parses_shortcuts() {
        let s = parse_shortcut("Cmd+Shift+L").unwrap();
        assert_eq!(s.modifiers, Modifiers::COMMAND.plus(Modifiers::SHIFT));
        assert_eq!(s.logical_key, Key::L);
        assert_eq!(parse_shortcut("alt+f7").unwrap().logical_key, Key::F7);
        assert_eq!(parse_shortcut("F6").unwrap().modifiers, Modifiers::NONE);
        assert_eq!(
            parse_shortcut("Ctrl+Alt+Space").unwrap().logical_key,
            Key::Space
        );
        assert_eq!(parse_shortcut("Cmd++").unwrap().logical_key, Key::Plus);
        assert!(parse_shortcut("L").is_err());
        assert!(parse_shortcut("Shift+L").is_err());
        assert!(parse_shortcut("Hyper+L").is_err());
    }

    #[test]
    fn file_type_matching() {
        let mut m = with_command("id = \"a\"\ntitle = \"A\"\nprogram = \"x\"").unwrap();
        let c = &mut m.commands[0];
        assert!(c.applies_to(None));
        c.file_types = vec!["rs".into(), ".TOML".into(), "Dockerfile".into()];
        assert!(c.applies_to(Some("main.rs")));
        assert!(c.applies_to(Some("Cargo.toml")));
        assert!(c.applies_to(Some("Dockerfile")));
        assert!(!c.applies_to(Some("main.py")));
        assert!(!c.applies_to(None));
    }

    #[test]
    fn versions_compare() {
        assert!(parse_version("0.2") > parse_version("0.1.9"));
        assert_eq!(parse_version("v1.2.3-beta"), Some([1, 2, 3]));
        assert_eq!(parse_version("x"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
    }
}

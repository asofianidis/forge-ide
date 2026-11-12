//! Keyboard shortcuts: each command's defaults, with the user's
//! `[keybindings]` from `settings.toml` applied on top.
//!
//! ```toml
//! [keybindings]
//! toggle_terminal = "cmd+j"                         # replaces the default
//! duplicate_line = ["cmd+shift+d", "alt+shift+down"]
//! go_to_line = []                                   # no shortcut
//! ```
//!
//! `cmd` is Cmd on macOS and Ctrl elsewhere; `ctrl` is Ctrl everywhere.

use std::collections::{BTreeMap, HashMap};

use egui::{Key, KeyboardShortcut, Modifiers};
use serde::{Deserialize, Serialize};

use crate::commands::Command;

/// The shortcuts given for one command in `settings.toml`: a single
/// shortcut, or a list (empty to unbind).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Keys {
    One(String),
    Many(Vec<String>),
}

impl Keys {
    fn as_slice(&self) -> &[String] {
        match self {
            Keys::One(s) => std::slice::from_ref(s),
            Keys::Many(v) => v,
        }
    }
}

/// The shortcuts in effect for every command.
#[derive(Debug, Clone)]
pub struct Keymap {
    by_command: HashMap<Command, Vec<KeyboardShortcut>>,
    /// Every (command, shortcut) pair, most specific shortcut first.
    table: Vec<(Command, KeyboardShortcut)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(&BTreeMap::new()).0
    }
}

impl Keymap {
    /// The defaults with `bindings` applied, plus a message for each binding
    /// that couldn't be used (unknown command, bad shortcut, clash). Those
    /// commands keep their defaults.
    ///
    /// A shortcut the user gives a command is taken away from any command
    /// that had it by default.
    pub fn new(bindings: &BTreeMap<String, Keys>) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut custom: Vec<(Command, Vec<KeyboardShortcut>)> = Vec::new();
        for (id, keys) in bindings {
            let Some(cmd) = Command::from_id(id) else {
                errors.push(format!("unknown command \"{id}\""));
                continue;
            };
            let mut shortcuts = Vec::new();
            let errors_before = errors.len();
            for text in keys.as_slice() {
                match parse_shortcut(text) {
                    Ok(s) => {
                        let taken = custom
                            .iter()
                            .find(|(_, list)| list.iter().any(|t| same(t, &s)))
                            .map(|(c, _)| *c);
                        match taken {
                            Some(other) => errors.push(format!(
                                "\"{text}\" for {id} is already bound to {}",
                                other.id()
                            )),
                            None if !shortcuts.iter().any(|t| same(t, &s)) => shortcuts.push(s),
                            None => {}
                        }
                    }
                    Err(e) => errors.push(format!("{id}: {e}")),
                }
            }
            // A binding with a mistake is ignored as a whole, so the command
            // keeps its defaults rather than ending up half-configured.
            if errors.len() == errors_before {
                custom.push((cmd, shortcuts));
            }
        }

        let user_keys: Vec<KeyboardShortcut> =
            custom.iter().flat_map(|(_, l)| l.iter().copied()).collect();
        let mut by_command = HashMap::new();
        for &cmd in Command::ALL {
            let shortcuts = match custom.iter().find(|(c, _)| *c == cmd) {
                Some((_, list)) => list.clone(),
                None => cmd
                    .default_shortcuts()
                    .iter()
                    .filter(|s| !user_keys.iter().any(|u| same(u, s)))
                    .copied()
                    .collect(),
            };
            by_command.insert(cmd, shortcuts);
        }
        let mut table: Vec<_> = Command::ALL
            .iter()
            .flat_map(|c| by_command[c].iter().map(move |&s| (*c, s)))
            .collect();
        // egui matches shortcuts "logically" (Cmd+P also matches
        // Cmd+Shift+P), so shortcuts with more modifiers must be checked first.
        table.sort_by_key(|(_, s)| {
            let m = s.modifiers;
            std::cmp::Reverse(
                [m.alt, m.shift, m.ctrl || m.command || m.mac_cmd]
                    .iter()
                    .filter(|&&b| b)
                    .count(),
            )
        });
        (Self { by_command, table }, errors)
    }

    /// Every (command, shortcut) pair, in the order to check them.
    pub fn table(&self) -> &[(Command, KeyboardShortcut)] {
        &self.table
    }

    /// The shortcuts for `cmd`; the first is the one shown in menus.
    pub fn shortcuts(&self, cmd: Command) -> &[KeyboardShortcut] {
        self.by_command.get(&cmd).map_or(&[], Vec::as_slice)
    }

    /// The shortcut shown for `cmd`, if it has one.
    pub fn shortcut(&self, cmd: Command) -> Option<&KeyboardShortcut> {
        self.shortcuts(cmd).first()
    }
}

fn same(a: &KeyboardShortcut, b: &KeyboardShortcut) -> bool {
    a.logical_key == b.logical_key && a.modifiers == b.modifiers
}

/// Parses a shortcut like `cmd+shift+p`, `alt+up`, `f12` or `ctrl+``.
/// Modifiers: `cmd` (Cmd on macOS, Ctrl elsewhere), `ctrl`, `alt` / `option`,
/// `shift`. Names are case-insensitive.
pub fn parse_shortcut(text: &str) -> Result<KeyboardShortcut, String> {
    let text = text.trim();
    // The key is after the last `+`, unless the key is `+` itself.
    let (mods, key) = if let Some(rest) = text.strip_suffix("++") {
        (rest, "+")
    } else if text == "+" {
        ("", "+")
    } else {
        match text.rsplit_once('+') {
            Some((m, k)) => (m, k),
            None => ("", text),
        }
    };
    let mut modifiers = Modifiers::NONE;
    for m in mods.split('+').map(str::trim).filter(|m| !m.is_empty()) {
        let modifier = match m.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "mod" | "cmdorctrl" | "primary" => Modifiers::COMMAND,
            "ctrl" | "control" => Modifiers::CTRL,
            "alt" | "option" | "opt" => Modifiers::ALT,
            "shift" => Modifiers::SHIFT,
            _ => return Err(format!("unknown modifier \"{m}\" in \"{text}\"")),
        };
        modifiers = modifiers.plus(modifier);
    }
    let key = key.trim();
    if key.is_empty() {
        return Err(format!("no key in \"{text}\""));
    }
    let key = Key::from_name(key)
        .or_else(|| {
            Key::ALL
                .iter()
                .copied()
                .find(|k| k.name().eq_ignore_ascii_case(key))
        })
        .or_else(|| {
            // Aliases egui doesn't know, and other casings of its names.
            let lower = key.to_ascii_lowercase();
            let name = match lower.as_str() {
                "esc" => "Escape",
                "del" => "Delete",
                "pgup" => "PageUp",
                "pgdn" | "pgdown" => "PageDown",
                "return" => "Enter",
                "up" => "ArrowUp",
                "down" => "ArrowDown",
                "left" => "ArrowLeft",
                "right" => "ArrowRight",
                "space" => "Space",
                _ => return None,
            };
            Key::from_name(name)
        })
        .ok_or_else(|| format!("unknown key \"{key}\" in \"{text}\""))?;
    Ok(KeyboardShortcut::new(modifiers, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn bindings(pairs: &[(&str, Keys)]) -> BTreeMap<String, Keys> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn parses_shortcuts() {
        let p = |s| parse_shortcut(s).unwrap();
        assert_eq!(
            p("cmd+shift+p"),
            KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::P)
        );
        assert_eq!(
            p("Ctrl+`"),
            KeyboardShortcut::new(Modifiers::CTRL, Key::Backtick)
        );
        assert_eq!(
            p("alt+up"),
            KeyboardShortcut::new(Modifiers::ALT, Key::ArrowUp)
        );
        assert_eq!(p("F12"), KeyboardShortcut::new(Modifiers::NONE, Key::F12));
        assert_eq!(p("f12"), KeyboardShortcut::new(Modifiers::NONE, Key::F12));
        assert_eq!(
            p("cmd++"),
            KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus)
        );
        assert_eq!(
            p("cmd+plus"),
            KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus)
        );
        assert_eq!(
            p("Option+Esc"),
            KeyboardShortcut::new(Modifiers::ALT, Key::Escape)
        );
        assert_eq!(
            p("pagedown"),
            KeyboardShortcut::new(Modifiers::NONE, Key::PageDown)
        );
        assert_eq!(
            p("cmd+,"),
            KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma)
        );
        assert!(parse_shortcut("hyper+k").is_err());
        assert!(parse_shortcut("cmd+").is_err());
        assert!(parse_shortcut("cmd+nope").is_err());
    }

    #[test]
    fn defaults_are_unique() {
        let mut seen = HashSet::new();
        for (cmd, s) in Keymap::default().table() {
            assert!(
                seen.insert((s.modifiers, s.logical_key)),
                "{cmd:?} reuses a shortcut"
            );
        }
    }

    #[test]
    fn more_specific_shortcuts_come_first() {
        let keymap = Keymap::default();
        let table = keymap.table();
        let pos = |c| table.iter().position(|(x, _)| *x == c).unwrap();
        assert!(pos(Command::ShowCommandPalette) < pos(Command::QuickOpen));
        assert!(pos(Command::FindInProject) < pos(Command::Find));
        assert!(pos(Command::DuplicateLine) < pos(Command::MoveLineDown));
        assert!(pos(Command::PreviousChange) < pos(Command::NextChange));
        assert!(pos(Command::FindReferences) < pos(Command::GoToDefinition));
        assert!(pos(Command::PreviousProblem) < pos(Command::NextProblem));
        // Cmd+Z would also match Cmd+Shift+Z.
        assert!(pos(Command::Redo) < pos(Command::Undo));
        assert!(pos(Command::SaveAs) < pos(Command::Save));
        assert!(pos(Command::PreviousTab) < pos(Command::NextTab));
    }

    /// egui's `consume_shortcut` ignores an extra Shift, so the plain
    /// shortcut would swallow its Shift variant if it were checked first.
    #[test]
    fn shift_variants_are_not_swallowed() {
        let custom = bindings(&[("find_in_project", Keys::One("cmd+shift+k".into()))]);
        for keymap in [Keymap::default(), Keymap::new(&custom).0] {
            let table = keymap.table();
            for (i, (cmd, s)) in table.iter().enumerate() {
                for (other, earlier) in &table[..i] {
                    assert!(
                        !s.modifiers.matches_logically(earlier.modifiers)
                            || s.logical_key != earlier.logical_key
                            || s.modifiers == earlier.modifiers,
                        "{other:?} is checked before {cmd:?} and would swallow it"
                    );
                }
            }
        }
    }

    #[test]
    fn user_bindings_replace_defaults() {
        let custom = bindings(&[
            ("toggle_terminal", Keys::One("cmd+j".into())),
            // Taken from Quick Open, which then has no shortcut.
            (
                "go_to_line",
                Keys::Many(vec!["cmd+p".into(), "ctrl+l".into()]),
            ),
            ("find", Keys::Many(vec![])),
        ]);
        let (keymap, errors) = Keymap::new(&custom);
        assert!(errors.is_empty(), "{errors:?}");
        let cmd_j = parse_shortcut("cmd+j").unwrap();
        assert_eq!(keymap.shortcuts(Command::ToggleTerminal), &[cmd_j]);
        assert_eq!(keymap.shortcuts(Command::GoToLine).len(), 2);
        assert!(keymap.shortcuts(Command::QuickOpen).is_empty());
        assert!(keymap.shortcut(Command::Find).is_none());
        assert!(!keymap.table().iter().any(|(c, _)| *c == Command::Find));
        // Untouched commands keep their defaults.
        assert_eq!(
            keymap.shortcuts(Command::Save),
            Command::Save.default_shortcuts()
        );
    }

    #[test]
    fn reports_bad_bindings() {
        let custom = bindings(&[
            ("no_such_command", Keys::One("cmd+j".into())),
            ("save", Keys::One("cmd+nope".into())),
            ("find", Keys::One("cmd+k".into())),
            ("replace", Keys::One("cmd+k".into())),
        ]);
        let (keymap, errors) = Keymap::new(&custom);
        assert_eq!(errors.len(), 3, "{errors:?}");
        // Commands with a bad binding keep their defaults.
        assert_eq!(
            keymap.shortcuts(Command::Save),
            Command::Save.default_shortcuts()
        );
        let cmd_k = parse_shortcut("cmd+k").unwrap();
        assert_eq!(keymap.shortcuts(Command::Find), &[cmd_k]);
        assert_eq!(
            keymap.shortcuts(Command::Replace),
            Command::Replace.default_shortcuts()
        );
    }

    #[test]
    fn keys_deserialize_from_string_or_list() {
        #[derive(Deserialize)]
        struct T {
            keybindings: BTreeMap<String, Keys>,
        }
        let t: T = toml::from_str(
            "[keybindings]\nsave = \"cmd+s\"\nfind = [\"cmd+f\", \"f3\"]\nquit = []",
        )
        .unwrap();
        assert_eq!(t.keybindings["save"], Keys::One("cmd+s".into()));
        assert_eq!(t.keybindings["find"].as_slice().len(), 2);
        assert_eq!(t.keybindings["quit"], Keys::Many(vec![]));
    }
}

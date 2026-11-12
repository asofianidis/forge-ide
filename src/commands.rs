//! Every user-facing action, with its name and keyboard shortcut.
//!
//! Menus, keyboard shortcuts and the command palette are all built from this
//! list, so an action added here is reachable all three ways.

use egui::{Key, KeyboardShortcut, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    NewFile,
    OpenFile,
    OpenFolder,
    Save,
    CloseTab,
    Quit,
    Find,
    Replace,
    FindInProject,
    ToggleComment,
    Indent,
    Outdent,
    DuplicateLine,
    MoveLineUp,
    MoveLineDown,
    QuickOpen,
    GoToLine,
    ShowCommandPalette,
    ShowExplorer,
    SelectTheme,
    ReloadThemes,
    OpenThemesFolder,
    LanguageServers,
    CheckForUpdates,
}

const fn shortcut(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(modifiers, key)
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CMD_ALT: Modifiers = Modifiers::COMMAND.plus(Modifiers::ALT);
const ALT: Modifiers = Modifiers::ALT;
const ALT_SHIFT: Modifiers = Modifiers::ALT.plus(Modifiers::SHIFT);

impl Command {
    pub const ALL: &[Command] = &[
        Command::NewFile,
        Command::OpenFile,
        Command::OpenFolder,
        Command::Save,
        Command::CloseTab,
        Command::Quit,
        Command::Find,
        Command::Replace,
        Command::FindInProject,
        Command::ToggleComment,
        Command::Indent,
        Command::Outdent,
        Command::DuplicateLine,
        Command::MoveLineUp,
        Command::MoveLineDown,
        Command::QuickOpen,
        Command::GoToLine,
        Command::ShowCommandPalette,
        Command::ShowExplorer,
        Command::SelectTheme,
        Command::ReloadThemes,
        Command::OpenThemesFolder,
        Command::LanguageServers,
        Command::CheckForUpdates,
    ];

    /// Name shown in menus.
    pub fn label(self) -> &'static str {
        match self {
            Command::NewFile => "New File",
            Command::OpenFile => "Open File…",
            Command::OpenFolder => "Open Folder…",
            Command::Save => "Save",
            Command::CloseTab => "Close Tab",
            Command::Quit => "Quit",
            Command::Find => "Find",
            Command::Replace => "Replace",
            Command::FindInProject => "Find in Project",
            Command::ToggleComment => "Toggle Line Comment",
            Command::Indent => "Indent Line",
            Command::Outdent => "Outdent Line",
            Command::DuplicateLine => "Duplicate Line",
            Command::MoveLineUp => "Move Line Up",
            Command::MoveLineDown => "Move Line Down",
            Command::QuickOpen => "Go to File…",
            Command::GoToLine => "Go to Line…",
            Command::ShowCommandPalette => "Command Palette…",
            Command::ShowExplorer => "Explorer",
            Command::SelectTheme => "Color Theme…",
            Command::ReloadThemes => "Reload Themes",
            Command::OpenThemesFolder => "Open Themes Folder",
            Command::LanguageServers => "Language Servers…",
            Command::CheckForUpdates => "Check for Updates",
        }
    }

    /// Menu the command lives in, used as a prefix in the command palette.
    pub fn category(self) -> &'static str {
        use Command::*;
        match self {
            NewFile | OpenFile | OpenFolder | Save | CloseTab | Quit => "File",
            Find | Replace | FindInProject | ToggleComment | Indent | Outdent | DuplicateLine
            | MoveLineUp | MoveLineDown => "Edit",
            QuickOpen | GoToLine => "Go",
            ShowCommandPalette | ShowExplorer | SelectTheme | ReloadThemes | OpenThemesFolder => {
                "View"
            }
            LanguageServers => "Tools",
            CheckForUpdates => "Help",
        }
    }

    /// Name shown in the command palette.
    pub fn palette_label(self) -> String {
        format!(
            "{}: {}",
            self.category(),
            self.label().trim_end_matches('…')
        )
    }

    /// Shortcuts that trigger this command; the first is shown in menus.
    pub fn shortcuts(self) -> &'static [KeyboardShortcut] {
        const fn one(s: KeyboardShortcut) -> [KeyboardShortcut; 1] {
            [s]
        }
        match self {
            Command::NewFile => &const { one(shortcut(CMD, Key::N)) },
            Command::OpenFile => &const { one(shortcut(CMD, Key::O)) },
            Command::Save => &const { one(shortcut(CMD, Key::S)) },
            Command::CloseTab => &const { one(shortcut(CMD, Key::W)) },
            Command::Find => &const { one(shortcut(CMD, Key::F)) },
            // macOS reserves Cmd+H for "Hide", so the menu shows Cmd+Option+F
            // there (as other editors do) and Cmd+H is kept as an alternative.
            Command::Replace => {
                if cfg!(target_os = "macos") {
                    &const { [shortcut(CMD_ALT, Key::F), shortcut(CMD, Key::H)] }
                } else {
                    &const { one(shortcut(CMD, Key::H)) }
                }
            }
            Command::FindInProject => &const { one(shortcut(CMD_SHIFT, Key::F)) },
            Command::ToggleComment => &const { one(shortcut(CMD, Key::Slash)) },
            Command::Indent => &const { one(shortcut(CMD, Key::CloseBracket)) },
            Command::Outdent => &const { one(shortcut(CMD, Key::OpenBracket)) },
            Command::DuplicateLine => &const { one(shortcut(ALT_SHIFT, Key::ArrowDown)) },
            Command::MoveLineUp => &const { one(shortcut(ALT, Key::ArrowUp)) },
            Command::MoveLineDown => &const { one(shortcut(ALT, Key::ArrowDown)) },
            Command::QuickOpen => &const { one(shortcut(CMD, Key::P)) },
            // Ctrl+G on every platform; Cmd+G is "find next" on macOS.
            Command::GoToLine => &const { one(shortcut(Modifiers::CTRL, Key::G)) },
            Command::ShowCommandPalette => &const { one(shortcut(CMD_SHIFT, Key::P)) },
            Command::ShowExplorer => &const { one(shortcut(CMD_SHIFT, Key::E)) },
            _ => &[],
        }
    }

    /// Commands that edit the active buffer. Their shortcuts only fire while
    /// the editor (rather than, say, the find field) has keyboard focus.
    pub fn edits_text(self) -> bool {
        matches!(
            self,
            Command::ToggleComment
                | Command::Indent
                | Command::Outdent
                | Command::DuplicateLine
                | Command::MoveLineUp
                | Command::MoveLineDown
        )
    }

    /// Commands paired with their shortcuts, most specific first.
    ///
    /// egui matches shortcuts "logically" (Cmd+P also matches Cmd+Shift+P), so
    /// shortcuts with more modifiers must be checked first.
    pub fn shortcut_table() -> Vec<(Command, KeyboardShortcut)> {
        let mut table: Vec<_> = Self::ALL
            .iter()
            .flat_map(|&c| c.shortcuts().iter().map(move |&s| (c, s)))
            .collect();
        table.sort_by_key(|(_, s)| {
            let m = s.modifiers;
            std::cmp::Reverse(
                [m.alt, m.shift, m.ctrl || m.command || m.mac_cmd]
                    .iter()
                    .filter(|&&b| b)
                    .count(),
            )
        });
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn shortcuts_are_unique() {
        let mut seen = HashSet::new();
        for (cmd, s) in Command::shortcut_table() {
            assert!(
                seen.insert((s.modifiers, s.logical_key)),
                "{cmd:?} reuses a shortcut"
            );
        }
    }

    #[test]
    fn more_specific_shortcuts_come_first() {
        let table = Command::shortcut_table();
        let pos = |c| table.iter().position(|(x, _)| *x == c).unwrap();
        assert!(pos(Command::ShowCommandPalette) < pos(Command::QuickOpen));
        assert!(pos(Command::FindInProject) < pos(Command::Find));
        assert!(pos(Command::DuplicateLine) < pos(Command::MoveLineDown));
    }

    #[test]
    fn every_command_is_listed() {
        let unique: HashSet<_> = Command::ALL.iter().collect();
        assert_eq!(unique.len(), Command::ALL.len());
        assert_eq!(Command::Find.palette_label(), "Edit: Find");
    }
}

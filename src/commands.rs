//! Application commands: the single list of actions shared by keyboard
//! shortcuts, menus and the command palette.

use egui::{Key, KeyboardShortcut, Modifiers};

const fn shortcut(modifiers: Modifiers, key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(modifiers, key))
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const REDO_ALT: KeyboardShortcut = KeyboardShortcut::new(CMD_SHIFT, Key::Z);
const PALETTE_ALT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F1);
const ZOOM_IN_ALT: KeyboardShortcut = KeyboardShortcut::new(CMD, Key::Plus);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewFile,
    OpenFile,
    OpenFolder,
    Save,
    SaveAs,
    CloseTab,
    Quit,
    Undo,
    Redo,
    Find,
    Replace,
    FindNext,
    FindPrevious,
    GoToLine,
    NextTab,
    PreviousTab,
    ShowPalette,
    OpenSettings,
    OpenSettingsFile,
    ToggleWordWrap,
    ToggleLineNumbers,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    ToggleTerminal,
    RestartTerminal,
}

impl Command {
    /// Every command, in the order shown by the palette when the query is empty.
    pub const ALL: &[Command] = &[
        Command::ShowPalette,
        Command::NewFile,
        Command::OpenFile,
        Command::OpenFolder,
        Command::Save,
        Command::SaveAs,
        Command::CloseTab,
        Command::NextTab,
        Command::PreviousTab,
        Command::Undo,
        Command::Redo,
        Command::Find,
        Command::Replace,
        Command::FindNext,
        Command::FindPrevious,
        Command::GoToLine,
        Command::ToggleWordWrap,
        Command::ToggleLineNumbers,
        Command::IncreaseFontSize,
        Command::DecreaseFontSize,
        Command::ResetFontSize,
        Command::ToggleTerminal,
        Command::RestartTerminal,
        Command::OpenSettings,
        Command::OpenSettingsFile,
        Command::Quit,
    ];

    /// Name shown in the command palette.
    pub fn label(self) -> &'static str {
        match self {
            Command::NewFile => "File: New File",
            Command::OpenFile => "File: Open File…",
            Command::OpenFolder => "File: Open Folder…",
            Command::Save => "File: Save",
            Command::SaveAs => "File: Save As…",
            Command::CloseTab => "File: Close Tab",
            Command::Quit => "File: Quit",
            Command::Undo => "Edit: Undo",
            Command::Redo => "Edit: Redo",
            Command::Find => "Edit: Find",
            Command::Replace => "Edit: Replace",
            Command::FindNext => "Edit: Find Next",
            Command::FindPrevious => "Edit: Find Previous",
            Command::GoToLine => "Go: Go to Line…",
            Command::NextTab => "View: Next Tab",
            Command::PreviousTab => "View: Previous Tab",
            Command::ShowPalette => "View: Command Palette",
            Command::OpenSettings => "Preferences: Settings…",
            Command::OpenSettingsFile => "Preferences: Open Settings File (TOML)",
            Command::ToggleWordWrap => "View: Toggle Word Wrap",
            Command::ToggleLineNumbers => "View: Toggle Line Numbers",
            Command::IncreaseFontSize => "View: Increase Font Size",
            Command::DecreaseFontSize => "View: Decrease Font Size",
            Command::ResetFontSize => "View: Reset Font Size",
            Command::ToggleTerminal => "Terminal: Toggle Terminal",
            Command::RestartTerminal => "Terminal: Restart Shell",
        }
    }

    /// Short name for menus (the label without its category).
    pub fn menu_label(self) -> &'static str {
        let label = self.label();
        label.split_once(": ").map_or(label, |(_, name)| name)
    }

    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        match self {
            Command::NewFile => shortcut(CMD, Key::N),
            Command::OpenFile => shortcut(CMD, Key::O),
            Command::OpenFolder => None,
            Command::Save => shortcut(CMD, Key::S),
            Command::SaveAs => shortcut(CMD_SHIFT, Key::S),
            Command::CloseTab => shortcut(CMD, Key::W),
            Command::Quit => shortcut(CMD, Key::Q),
            Command::Undo => shortcut(CMD, Key::Z),
            Command::Redo => shortcut(CMD, Key::Y),
            Command::Find => shortcut(CMD, Key::F),
            Command::Replace => shortcut(CMD, Key::H),
            Command::FindNext => shortcut(Modifiers::NONE, Key::F3),
            Command::FindPrevious => shortcut(Modifiers::SHIFT, Key::F3),
            Command::GoToLine => shortcut(CMD, Key::G),
            Command::NextTab => shortcut(Modifiers::CTRL, Key::Tab),
            Command::PreviousTab => shortcut(Modifiers::CTRL.plus(Modifiers::SHIFT), Key::Tab),
            Command::ShowPalette => shortcut(CMD_SHIFT, Key::P),
            Command::OpenSettings => shortcut(CMD, Key::Comma),
            Command::OpenSettingsFile => None,
            Command::ToggleWordWrap => shortcut(Modifiers::ALT, Key::Z),
            Command::ToggleLineNumbers => None,
            Command::IncreaseFontSize => shortcut(CMD, Key::Equals),
            Command::DecreaseFontSize => shortcut(CMD, Key::Minus),
            Command::ResetFontSize => shortcut(CMD, Key::Num0),
            Command::ToggleTerminal => shortcut(Modifiers::CTRL, Key::Backtick),
            Command::RestartTerminal => None,
        }
    }

    /// Extra shortcuts that also trigger the command.
    pub fn alt_shortcuts(self) -> &'static [KeyboardShortcut] {
        match self {
            Command::Redo => &[REDO_ALT],
            Command::ShowPalette => &[PALETTE_ALT],
            Command::IncreaseFontSize => &[ZOOM_IN_ALT],
            _ => &[],
        }
    }

    /// Commands that act on the editor's own text and must not steal keys from
    /// other focused text fields (which have their own undo/redo).
    pub fn needs_editor_focus(self) -> bool {
        matches!(self, Command::Undo | Command::Redo)
    }

    /// All (shortcut, command) pairs, ordered so shortcuts with more modifiers are
    /// checked first: `consume_shortcut` ignores extra Shift, so Ctrl+Z would
    /// otherwise also swallow Ctrl+Shift+Z.
    pub fn shortcut_table() -> Vec<(KeyboardShortcut, Command)> {
        let mut table: Vec<_> = Self::ALL
            .iter()
            .flat_map(|&cmd| {
                cmd.shortcut()
                    .into_iter()
                    .chain(cmd.alt_shortcuts().iter().copied())
                    .map(move |s| (s, cmd))
            })
            .collect();
        table.sort_by_key(|(s, _)| std::cmp::Reverse(modifier_count(s.modifiers)));
        table
    }
}

fn modifier_count(m: Modifiers) -> u8 {
    u8::from(m.alt) + u8::from(m.ctrl) + u8::from(m.shift) + u8::from(m.command || m.mac_cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_listed_once() {
        for (i, a) in Command::ALL.iter().enumerate() {
            assert!(!Command::ALL[i + 1..].contains(a), "{a:?} listed twice");
        }
    }

    #[test]
    fn shortcuts_are_unique() {
        let table = Command::shortcut_table();
        for (i, (a, ca)) in table.iter().enumerate() {
            for (b, cb) in &table[i + 1..] {
                assert!(a != b, "{ca:?} and {cb:?} share a shortcut");
            }
        }
    }

    #[test]
    fn shift_variants_are_checked_first() {
        let table = Command::shortcut_table();
        let pos = |s: KeyboardShortcut| table.iter().position(|(t, _)| *t == s).unwrap();
        assert!(
            pos(KeyboardShortcut::new(CMD_SHIFT, Key::Z)) < pos(KeyboardShortcut::new(CMD, Key::Z))
        );
        assert!(
            pos(KeyboardShortcut::new(CMD_SHIFT, Key::S)) < pos(KeyboardShortcut::new(CMD, Key::S))
        );
    }

    #[test]
    fn menu_label_strips_category() {
        assert_eq!(Command::Save.menu_label(), "Save");
        assert_eq!(Command::GoToLine.menu_label(), "Go to Line…");
    }
}

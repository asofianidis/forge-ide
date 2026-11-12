//! Every user-facing action, with its name and keyboard shortcut.
//!
//! Menus, keyboard shortcuts and the command palette are all built from this
//! list, so an action added here is reachable all three ways. The shortcuts
//! here are the defaults; see [`crate::keymap`] for the user's own.

use egui::{Key, KeyboardShortcut, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    NewFile,
    OpenFile,
    OpenFolder,
    OpenRecent,
    Save,
    SaveAs,
    CloseTab,
    RevealActiveFile,
    OpenSettings,
    OpenSettingsFile,
    ShowKeybindings,
    Quit,
    Undo,
    Redo,
    Find,
    Replace,
    FindInProject,
    ToggleComment,
    Indent,
    Outdent,
    DuplicateLine,
    MoveLineUp,
    MoveLineDown,
    AddCursorAbove,
    AddCursorBelow,
    AddNextOccurrence,
    SelectAllOccurrences,
    AddCursorsToLineEnds,
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
    TriggerSuggest,
    TriggerParameterHints,
    QuickFix,
    RenameSymbol,
    FormatDocument,
    QuickOpen,
    GoToLine,
    GoToSymbol,
    GoToWorkspaceSymbol,
    NextTab,
    PreviousTab,
    GoToDefinition,
    FindReferences,
    NextProblem,
    PreviousProblem,
    ShowCommandPalette,
    ShowExplorer,
    ShowSourceControl,
    ShowPullRequests,
    ShowExtensions,
    ShowProblems,
    ToggleBlame,
    NextChange,
    PreviousChange,
    FileHistory,
    RepositoryHistory,
    ToggleWordWrap,
    ToggleLineNumbers,
    SplitEditor,
    MoveTabToNextGroup,
    FocusFirstGroup,
    FocusSecondGroup,
    FocusThirdGroup,
    ToggleInlayHints,
    ToggleBreadcrumbs,
    ToggleMinimap,
    ToggleMarkdownPreview,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    SelectTheme,
    ReloadThemes,
    OpenThemesFolder,
    LanguageServers,
    RestartLanguageServers,
    ToggleFormatOnSave,
    InstallExtension,
    ReloadExtensions,
    OpenExtensionsFolder,
    ExtensionGuide,
    ToggleTerminal,
    RestartTerminal,
    CheckForUpdates,
}

/// The platform's name for showing a file in its file manager.
pub const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else if cfg!(target_os = "windows") {
    "Reveal in File Explorer"
} else {
    "Open Containing Folder"
};

const fn shortcut(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(modifiers, key)
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CMD_ALT: Modifiers = Modifiers::COMMAND.plus(Modifiers::ALT);
const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);
const ALT: Modifiers = Modifiers::ALT;
const ALT_SHIFT: Modifiers = Modifiers::ALT.plus(Modifiers::SHIFT);

impl Command {
    pub const ALL: &[Command] = &[
        Command::NewFile,
        Command::OpenFile,
        Command::OpenFolder,
        Command::OpenRecent,
        Command::Save,
        Command::SaveAs,
        Command::CloseTab,
        Command::RevealActiveFile,
        Command::OpenSettings,
        Command::OpenSettingsFile,
        Command::ShowKeybindings,
        Command::Quit,
        Command::Undo,
        Command::Redo,
        Command::Find,
        Command::Replace,
        Command::FindInProject,
        Command::ToggleComment,
        Command::Indent,
        Command::Outdent,
        Command::DuplicateLine,
        Command::MoveLineUp,
        Command::MoveLineDown,
        Command::AddCursorAbove,
        Command::AddCursorBelow,
        Command::AddNextOccurrence,
        Command::SelectAllOccurrences,
        Command::AddCursorsToLineEnds,
        Command::Fold,
        Command::Unfold,
        Command::FoldAll,
        Command::UnfoldAll,
        Command::TriggerSuggest,
        Command::TriggerParameterHints,
        Command::QuickFix,
        Command::RenameSymbol,
        Command::FormatDocument,
        Command::QuickOpen,
        Command::GoToLine,
        Command::GoToSymbol,
        Command::GoToWorkspaceSymbol,
        Command::NextTab,
        Command::PreviousTab,
        Command::GoToDefinition,
        Command::FindReferences,
        Command::NextProblem,
        Command::PreviousProblem,
        Command::ShowCommandPalette,
        Command::ShowExplorer,
        Command::ShowSourceControl,
        Command::ShowPullRequests,
        Command::ShowExtensions,
        Command::ShowProblems,
        Command::ToggleBlame,
        Command::NextChange,
        Command::PreviousChange,
        Command::FileHistory,
        Command::RepositoryHistory,
        Command::ToggleWordWrap,
        Command::ToggleLineNumbers,
        Command::SplitEditor,
        Command::MoveTabToNextGroup,
        Command::FocusFirstGroup,
        Command::FocusSecondGroup,
        Command::FocusThirdGroup,
        Command::ToggleInlayHints,
        Command::ToggleBreadcrumbs,
        Command::ToggleMinimap,
        Command::ToggleMarkdownPreview,
        Command::IncreaseFontSize,
        Command::DecreaseFontSize,
        Command::ResetFontSize,
        Command::SelectTheme,
        Command::ReloadThemes,
        Command::OpenThemesFolder,
        Command::LanguageServers,
        Command::RestartLanguageServers,
        Command::ToggleFormatOnSave,
        Command::InstallExtension,
        Command::ReloadExtensions,
        Command::OpenExtensionsFolder,
        Command::ExtensionGuide,
        Command::ToggleTerminal,
        Command::RestartTerminal,
        Command::CheckForUpdates,
    ];

    /// Name shown in menus.
    pub fn label(self) -> &'static str {
        match self {
            Command::NewFile => "New File",
            Command::OpenFile => "Open File…",
            Command::OpenFolder => "Open Folder…",
            Command::OpenRecent => "Open Recent Folder…",
            Command::Save => "Save",
            Command::SaveAs => "Save As…",
            Command::CloseTab => "Close Tab",
            Command::RevealActiveFile => REVEAL_LABEL,
            Command::OpenSettings => "Settings…",
            Command::OpenSettingsFile => "Open Settings File (TOML)",
            Command::ShowKeybindings => "Keyboard Shortcuts…",
            Command::Quit => "Quit",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::Find => "Find",
            Command::Replace => "Replace",
            Command::FindInProject => "Find in Project",
            Command::ToggleComment => "Toggle Line Comment",
            Command::Indent => "Indent Line",
            Command::Outdent => "Outdent Line",
            Command::DuplicateLine => "Duplicate Line",
            Command::MoveLineUp => "Move Line Up",
            Command::MoveLineDown => "Move Line Down",
            Command::AddCursorAbove => "Add Cursor Above",
            Command::AddCursorBelow => "Add Cursor Below",
            Command::AddNextOccurrence => "Add Next Occurrence",
            Command::SelectAllOccurrences => "Select All Occurrences",
            Command::AddCursorsToLineEnds => "Add Cursors to Line Ends",
            Command::Fold => "Fold",
            Command::Unfold => "Unfold",
            Command::FoldAll => "Fold All",
            Command::UnfoldAll => "Unfold All",
            Command::TriggerSuggest => "Trigger Suggest",
            Command::TriggerParameterHints => "Trigger Parameter Hints",
            Command::QuickFix => "Quick Fix…",
            Command::RenameSymbol => "Rename Symbol…",
            Command::FormatDocument => "Format Document",
            Command::QuickOpen => "Go to File…",
            Command::GoToLine => "Go to Line…",
            Command::GoToSymbol => "Go to Symbol in File…",
            Command::GoToWorkspaceSymbol => "Go to Symbol in Workspace…",
            Command::NextTab => "Next Tab",
            Command::PreviousTab => "Previous Tab",
            Command::GoToDefinition => "Go to Definition",
            Command::FindReferences => "Find All References",
            Command::NextProblem => "Next Problem",
            Command::PreviousProblem => "Previous Problem",
            Command::ShowCommandPalette => "Command Palette…",
            Command::ShowExplorer => "Explorer",
            Command::ShowSourceControl => "Source Control",
            Command::ShowPullRequests => "Pull Requests",
            Command::ShowExtensions => "Extensions",
            Command::ShowProblems => "Problems…",
            Command::ToggleBlame => "Toggle Blame",
            Command::NextChange => "Next Change",
            Command::PreviousChange => "Previous Change",
            Command::FileHistory => "File History",
            Command::RepositoryHistory => "Repository History",
            Command::ToggleWordWrap => "Toggle Word Wrap",
            Command::ToggleLineNumbers => "Toggle Line Numbers",
            Command::SplitEditor => "Split Editor",
            Command::MoveTabToNextGroup => "Move Tab to Next Group",
            Command::FocusFirstGroup => "Focus First Editor Group",
            Command::FocusSecondGroup => "Focus Second Editor Group",
            Command::FocusThirdGroup => "Focus Third Editor Group",
            Command::ToggleInlayHints => "Toggle Inlay Hints",
            Command::ToggleBreadcrumbs => "Toggle Breadcrumbs",
            Command::ToggleMinimap => "Toggle Minimap",
            Command::ToggleMarkdownPreview => "Markdown Preview",
            Command::IncreaseFontSize => "Increase Font Size",
            Command::DecreaseFontSize => "Decrease Font Size",
            Command::ResetFontSize => "Reset Font Size",
            Command::SelectTheme => "Color Theme…",
            Command::ReloadThemes => "Reload Themes",
            Command::OpenThemesFolder => "Open Themes Folder",
            Command::LanguageServers => "Language Servers…",
            Command::RestartLanguageServers => "Restart Language Servers",
            Command::ToggleFormatOnSave => "Format on Save",
            Command::InstallExtension => "Install Extension from ZIP…",
            Command::ReloadExtensions => "Reload Extensions",
            Command::OpenExtensionsFolder => "Open Extensions Folder",
            Command::ExtensionGuide => "Extension Authoring Guide",
            Command::ToggleTerminal => "Toggle Terminal",
            Command::RestartTerminal => "Restart Shell",
            Command::CheckForUpdates => "Check for Updates",
        }
    }

    /// Menu the command lives in, used as a prefix in the command palette.
    pub fn category(self) -> &'static str {
        use Command::*;
        match self {
            NewFile | OpenFile | OpenFolder | OpenRecent | Save | SaveAs | CloseTab
            | RevealActiveFile | OpenSettings | OpenSettingsFile | ShowKeybindings | Quit => "File",
            Undo
            | Redo
            | Find
            | Replace
            | FindInProject
            | ToggleComment
            | Indent
            | Outdent
            | DuplicateLine
            | MoveLineUp
            | MoveLineDown
            | TriggerSuggest
            | TriggerParameterHints
            | QuickFix
            | RenameSymbol
            | FormatDocument => "Edit",
            AddCursorAbove | AddCursorBelow | AddNextOccurrence | SelectAllOccurrences
            | AddCursorsToLineEnds => "Selection",
            Fold | Unfold | FoldAll | UnfoldAll => "View",
            QuickOpen | GoToLine | GoToSymbol | GoToWorkspaceSymbol | NextTab | PreviousTab
            | GoToDefinition | FindReferences | NextProblem | PreviousProblem | NextChange
            | PreviousChange => "Go",
            ShowCommandPalette
            | ShowExplorer
            | ShowSourceControl
            | ShowPullRequests
            | ShowExtensions
            | ShowProblems
            | ToggleBlame
            | FileHistory
            | RepositoryHistory
            | ToggleWordWrap
            | ToggleLineNumbers
            | ToggleInlayHints
            | ToggleBreadcrumbs
            | ToggleMinimap
            | ToggleMarkdownPreview
            | SplitEditor
            | MoveTabToNextGroup
            | FocusFirstGroup
            | FocusSecondGroup
            | FocusThirdGroup
            | IncreaseFontSize
            | DecreaseFontSize
            | ResetFontSize
            | SelectTheme
            | ReloadThemes
            | OpenThemesFolder => "View",
            LanguageServers | RestartLanguageServers | ToggleFormatOnSave => "Tools",
            InstallExtension | ReloadExtensions | OpenExtensionsFolder | ExtensionGuide => {
                "Extensions"
            }
            ToggleTerminal | RestartTerminal => "Terminal",
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

    /// Name used for the command in `settings.toml` keybindings: the variant
    /// name in snake_case, e.g. `toggle_terminal`.
    pub fn id(self) -> String {
        let mut id = String::new();
        for (i, c) in format!("{self:?}").chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    id.push('_');
                }
                id.push(c.to_ascii_lowercase());
            } else {
                id.push(c);
            }
        }
        id
    }

    /// The command with this [`Self::id`].
    pub fn from_id(id: &str) -> Option<Command> {
        Self::ALL.iter().copied().find(|c| c.id() == id)
    }

    /// Default shortcuts for this command; the first is shown in menus.
    /// Use [`crate::keymap::Keymap`] for the ones in effect.
    pub fn default_shortcuts(self) -> &'static [KeyboardShortcut] {
        const fn one(s: KeyboardShortcut) -> [KeyboardShortcut; 1] {
            [s]
        }
        match self {
            Command::NewFile => &const { one(shortcut(CMD, Key::N)) },
            Command::OpenFile => &const { one(shortcut(CMD, Key::O)) },
            Command::Save => &const { one(shortcut(CMD, Key::S)) },
            Command::SaveAs => &const { one(shortcut(CMD_SHIFT, Key::S)) },
            Command::CloseTab => &const { one(shortcut(CMD, Key::W)) },
            Command::Undo => &const { one(shortcut(CMD, Key::Z)) },
            // Cmd+Shift+Z is the usual redo on macOS, Ctrl+Y on Windows and Linux.
            Command::Redo => {
                if cfg!(target_os = "macos") {
                    &const { [shortcut(CMD_SHIFT, Key::Z), shortcut(CMD, Key::Y)] }
                } else {
                    &const { [shortcut(CMD, Key::Y), shortcut(CMD_SHIFT, Key::Z)] }
                }
            }
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
            // Ctrl+R on every platform, as in other editors.
            Command::OpenRecent => &const { one(shortcut(Modifiers::CTRL, Key::R)) },
            // Ctrl+G on every platform; Cmd+G is "find next" on macOS.
            Command::GoToLine => &const { one(shortcut(Modifiers::CTRL, Key::G)) },
            // Ctrl+Tab on every platform: Cmd+Tab switches apps on macOS.
            Command::NextTab => &const { one(shortcut(Modifiers::CTRL, Key::Tab)) },
            Command::PreviousTab => &const { one(shortcut(CTRL_SHIFT, Key::Tab)) },
            Command::ShowCommandPalette => &const { one(shortcut(CMD_SHIFT, Key::P)) },
            Command::ShowExplorer => &const { one(shortcut(CMD_SHIFT, Key::E)) },
            Command::ShowSourceControl => &const { one(shortcut(CMD_SHIFT, Key::G)) },
            // Cmd+Shift+P is the command palette, as in other editors.
            Command::ShowPullRequests => &const { one(shortcut(CMD_SHIFT, Key::R)) },
            Command::ShowExtensions => &const { one(shortcut(CMD_SHIFT, Key::X)) },
            Command::ToggleBlame => &const { one(shortcut(CMD_ALT, Key::B)) },
            Command::NextChange => &const { one(shortcut(ALT, Key::F5)) },
            // Ctrl+Space on every platform, as in other editors.
            Command::TriggerSuggest => &const { one(shortcut(Modifiers::CTRL, Key::Space)) },
            Command::TriggerParameterHints => &const { one(shortcut(CMD_SHIFT, Key::Space)) },
            Command::QuickFix => &const { one(shortcut(CMD, Key::Period)) },
            Command::GoToSymbol => &const { one(shortcut(CMD_SHIFT, Key::O)) },
            Command::GoToWorkspaceSymbol => &const { one(shortcut(CMD, Key::T)) },
            Command::RenameSymbol => &const { one(shortcut(Modifiers::NONE, Key::F2)) },
            Command::FormatDocument => &const { one(shortcut(CMD_SHIFT, Key::I)) },
            Command::GoToDefinition => &const { one(shortcut(Modifiers::NONE, Key::F12)) },
            Command::FindReferences => &const { one(shortcut(Modifiers::SHIFT, Key::F12)) },
            Command::NextProblem => &const { one(shortcut(Modifiers::NONE, Key::F8)) },
            Command::PreviousProblem => &const { one(shortcut(Modifiers::SHIFT, Key::F8)) },
            Command::ShowProblems => &const { one(shortcut(CMD_SHIFT, Key::M)) },
            Command::PreviousChange => &const { one(shortcut(ALT_SHIFT, Key::F5)) },
            Command::OpenSettings => &const { one(shortcut(CMD, Key::Comma)) },
            Command::ToggleWordWrap => &const { one(shortcut(ALT, Key::Z)) },
            Command::AddCursorAbove => &const { one(shortcut(CMD_ALT, Key::ArrowUp)) },
            Command::AddCursorBelow => &const { one(shortcut(CMD_ALT, Key::ArrowDown)) },
            Command::AddNextOccurrence => &const { one(shortcut(CMD, Key::D)) },
            Command::SelectAllOccurrences => &const { one(shortcut(CMD_SHIFT, Key::L)) },
            Command::AddCursorsToLineEnds => &const { one(shortcut(ALT_SHIFT, Key::I)) },
            Command::Fold => &const { one(shortcut(CMD_ALT, Key::OpenBracket)) },
            Command::Unfold => &const { one(shortcut(CMD_ALT, Key::CloseBracket)) },
            Command::SplitEditor => &const { one(shortcut(CMD, Key::Backslash)) },
            Command::FocusFirstGroup => &const { one(shortcut(CMD, Key::Num1)) },
            Command::FocusSecondGroup => &const { one(shortcut(CMD, Key::Num2)) },
            Command::FocusThirdGroup => &const { one(shortcut(CMD, Key::Num3)) },
            // As in other editors. Plain Cmd+V stays paste.
            Command::ToggleMarkdownPreview => &const { one(shortcut(CMD_SHIFT, Key::V)) },
            // Cmd+= and Cmd+Shift+= (Cmd++) both make the text bigger. These
            // replace egui's whole-UI keyboard zoom, which is turned off.
            Command::IncreaseFontSize => {
                &const { [shortcut(CMD, Key::Equals), shortcut(CMD, Key::Plus)] }
            }
            Command::DecreaseFontSize => &const { one(shortcut(CMD, Key::Minus)) },
            Command::ResetFontSize => &const { one(shortcut(CMD, Key::Num0)) },
            // Ctrl+` on every platform, as in other editors.
            Command::ToggleTerminal => &const { one(shortcut(Modifiers::CTRL, Key::Backtick)) },
            _ => &[],
        }
    }

    /// Commands that edit the active buffer. Their shortcuts only fire while
    /// the editor (rather than, say, the find field) has keyboard focus.
    pub fn edits_text(self) -> bool {
        matches!(
            self,
            Command::Undo
                | Command::Redo
                | Command::ToggleComment
                | Command::Indent
                | Command::Outdent
                | Command::DuplicateLine
                | Command::MoveLineUp
                | Command::MoveLineDown
                | Command::AddCursorAbove
                | Command::AddCursorBelow
                | Command::AddNextOccurrence
                | Command::SelectAllOccurrences
                | Command::AddCursorsToLineEnds
                | Command::Fold
                | Command::Unfold
                | Command::FoldAll
                | Command::UnfoldAll
                | Command::TriggerSuggest
                | Command::TriggerParameterHints
                | Command::QuickFix
                | Command::RenameSymbol
                | Command::FormatDocument
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_command_is_listed() {
        let unique: HashSet<_> = Command::ALL.iter().collect();
        assert_eq!(unique.len(), Command::ALL.len());
        assert_eq!(Command::Find.palette_label(), "Edit: Find");
        assert_eq!(Command::OpenSettings.palette_label(), "File: Settings");
        assert_eq!(
            Command::RestartTerminal.palette_label(),
            "Terminal: Restart Shell"
        );
    }

    #[test]
    fn ids_round_trip() {
        assert_eq!(Command::ToggleTerminal.id(), "toggle_terminal");
        assert_eq!(Command::Quit.id(), "quit");
        for &cmd in Command::ALL {
            assert_eq!(Command::from_id(&cmd.id()), Some(cmd));
        }
        assert_eq!(Command::from_id("ToggleTerminal"), None);
    }
}

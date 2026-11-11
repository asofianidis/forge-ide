# Forge IDE

A lightweight, native code editor written in Rust, built on [egui](https://github.com/emilk/egui).

> **Status:** early scaffold. Expect rough edges.

## Features

- File explorer with lazily-loaded directory tree
- Tabbed editing with unsaved-change indicators and a save prompt when closing tabs or the window
- Rope-backed buffers with grouped undo/redo history
- Find / replace with match-case and regex options (`$1` capture groups in replacements)
- Go to line[:column]
- Command palette with fuzzy matching over every command
- Integrated terminal (Ctrl+`) running your shell in a pseudo-terminal, with colors and scrollback
- Settings (Ctrl+,): color theme, font size, word wrap, line numbers, terminal shell, update checks
- Syntax highlighting for many languages (via [syntect](https://github.com/trishume/syntect))
- Line-number gutter with current-line highlight
- Cursor line/column in the status bar
- Native open/save dialogs (via [rfd](https://github.com/PolyMeilex/rfd))
- Themes: built-in **Blossom** (default) and **Blossom Light**, plus custom TOML themes
- Automatic updates from GitHub releases

## Install

Download the archive for your platform from the
[latest release](https://github.com/asofianidis/forge-ide/releases/latest), extract it, and
run `forge-ide` (`forge-ide.exe` on Windows). Builds are published for Windows (x64),
Linux (x64), and macOS (Apple Silicon and Intel).

> **macOS:** the binary isn't signed yet. If Gatekeeper blocks it, run
> `xattr -d com.apple.quarantine forge-ide` once.

## Updates

Release builds check for a new version on startup. When one is available, an
**Update to …** button appears in the status bar. Click it to download and install the new version
(verified against the release's `SHA256SUMS`), then **Restart to update**. Restarting is disabled
while you have unsaved changes.

You can also use **Help → Check for Updates**, or turn off the startup check under
**Help → Check for Updates on Startup**. Builds you compile yourself only check when you ask.

### Releases

Every push to `main` runs [`.github/workflows/release.yml`](.github/workflows/release.yml). It
checks and tests the code, builds all platforms, and publishes a GitHub release tagged
`v<major>.<minor>.<run number>`, using the major/minor from `Cargo.toml`, e.g. `v0.1.42`.
To start a new series, bump the major or minor version in `Cargo.toml`.

## Themes

Switch themes from **View → Theme**. Your choice is remembered between sessions.

Custom themes are TOML files in the themes folder (**View → Theme → Open Themes Folder**):

| OS      | Location                                          |
|---------|---------------------------------------------------|
| Linux   | `~/.config/forge-ide/themes/`                     |
| macOS   | `~/Library/Application Support/forge-ide/themes/` |
| Windows | `%APPDATA%\forge-ide\themes\`                     |

A theme only needs a `name`. Anything you leave out comes from the theme named in `extends`
(`"Blossom"` if you don't set one):

```toml
name = "My Theme"
extends = "Blossom Light"

[ui]
accent = "#0f9d8f"

[syntax]
keyword = "#0f9d8f"
```

See [`themes/example.toml`](themes/example.toml) for every key. After editing a file, use
**View → Theme → Reload Themes**. Files that fail to load are reported in the status bar.

## Getting started

Requires a recent stable Rust toolchain (edition 2024).

```sh
cargo run --release              # opens the current directory
cargo run --release -- path/to/project
```

### Keyboard shortcuts

| Action     | Shortcut |
|------------|----------|
| New file   | Ctrl+N   |
| Open file  | Ctrl+O   |
| Save       | Ctrl+S   |
| Save as    | Ctrl+Shift+S |
| Close tab  | Ctrl+W   |
| Undo       | Ctrl+Z   |
| Redo       | Ctrl+Y / Ctrl+Shift+Z |
| Find       | Ctrl+F   |
| Replace    | Ctrl+H   |
| Find next / previous | F3 / Shift+F3 |
| Go to line | Ctrl+G   |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Command palette | Ctrl+Shift+P / F1 |
| Settings   | Ctrl+,   |
| Font size bigger / smaller / reset | Ctrl+= / Ctrl+- / Ctrl+0 |
| Toggle word wrap | Alt+Z |
| Toggle terminal | Ctrl+` |
| Quit       | Ctrl+Q   |

(Cmd instead of Ctrl on macOS.)

### Settings

Settings are stored as TOML in `<config dir>/forge-ide/settings.toml`
(`%APPDATA%\forge-ide\settings.toml` on Windows, `~/.config/forge-ide/settings.toml` on Linux,
`~/Library/Application Support/forge-ide/settings.toml` on macOS). Set `FORGE_SETTINGS` to use a
different file. Changes made in the Settings window are saved immediately; editing the file in
Forge reloads it on save.

```toml
theme = "Blossom"         # any built-in or custom theme name
font_size = 14.0
word_wrap = false
line_numbers = true
auto_update = true        # check for updates on startup (default: on for release builds)
terminal_shell = "pwsh -NoLogo"  # optional; defaults to PowerShell on Windows, $SHELL elsewhere
```

## Project layout

```
src/
├── main.rs              # entry point, window setup
├── app.rs               # application state and UI layout
├── commands.rs          # command list: labels, shortcuts (shared by menus and palette)
├── find_bar.rs          # find/replace bar and go-to-line prompt
├── palette.rs           # command palette and fuzzy matcher
├── settings.rs          # persisted preferences (settings.toml) and settings window
├── terminal.rs          # integrated terminal (PTY + vt100 screen)
├── theme.rs             # built-in + custom themes
├── updater.rs           # self-update from GitHub releases
├── workspace.rs         # file tree / project explorer
└── editor/
    ├── buffer.rs        # rope-backed text buffer, undo/redo history
    ├── document.rs      # open file buffer, load/save, dirty tracking
    ├── gutter.rs        # line-number gutter
    ├── search.rs        # find/replace matching
    └── highlight.rs     # syntect → egui LayoutJob highlighting
```

## Roadmap

- [x] Line-number gutter
- [x] Rope-based buffer for large files, undo/redo history
- [x] Find / replace, go to line
- [x] Command palette
- [x] Unsaved-changes prompt on close
- [x] Integrated terminal
- [ ] LSP client (diagnostics, completion, go to definition)
- [x] Themes
- [x] Settings UI

## License

MIT, see [LICENSE](LICENSE).

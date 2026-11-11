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
- Settings (Ctrl+,): color theme (all bundled syntect themes; the UI follows light/dark), font size, word wrap, line numbers
- Syntax highlighting for many languages (via [syntect](https://github.com/trishume/syntect))
- Line-number gutter with current-line highlight
- Cursor line/column in the status bar
- Native open/save dialogs (via [rfd](https://github.com/PolyMeilex/rfd))

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
theme = "base16-ocean.dark"
font_size = 14.0
word_wrap = false
line_numbers = true
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
├── settings.rs          # persisted user settings
├── terminal.rs          # integrated terminal (PTY + vt100 screen)
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
- [x] Settings and themes

## License

MIT, see [LICENSE](LICENSE).

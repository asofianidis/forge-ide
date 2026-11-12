# Forge IDE

A lightweight, native code editor written in Rust, built on [egui](https://github.com/emilk/egui).

> **Status:** early scaffold. Expect rough edges.

## Features

- File explorer with lazily-loaded directory tree
- Tabbed editing with unsaved-change indicators
- Syntax highlighting for many languages (via [syntect](https://github.com/trishume/syntect))
- Line-number gutter with current-line highlight
- Cursor line/column in the status bar
- Native open/save dialogs (via [rfd](https://github.com/PolyMeilex/rfd))
- Language server browser (**Tools → Language Servers…**): finds servers for the
  languages in your project, shows which are installed, and installs missing ones
  with one click via the language's own toolchain (rustup, npm, go, pipx, …)
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
| Close tab  | Ctrl+W   |

(Cmd instead of Ctrl on macOS.)

## Project layout

```
src/
├── main.rs              # entry point, window setup
├── app.rs               # application state and UI layout
├── settings.rs          # persisted preferences (settings.toml)
├── theme.rs             # built-in + custom themes
├── updater.rs           # self-update from GitHub releases
├── workspace.rs         # file tree / project explorer, file-type scan
├── lsp/
│   ├── catalog.rs       # known language servers and their install commands
│   ├── detect.rs        # finding installed servers / package managers
│   ├── install.rs       # background install jobs
│   └── browser.rs       # "Language Servers" window
└── editor/
    ├── document.rs      # open file buffer, load/save, dirty tracking
    ├── gutter.rs        # line-number gutter
    └── highlight.rs     # syntect → egui LayoutJob highlighting
```

## Roadmap

- [x] Line-number gutter
- [ ] Rope-based buffer for large files, undo/redo history
- [ ] Find / replace, go to line
- [ ] Command palette
- [ ] Unsaved-changes prompt on close
- [ ] Integrated terminal
- [x] Language server discovery and installation
- [ ] LSP client (diagnostics, completion, go to definition)
- [x] Themes
- [ ] Settings UI

## License

MIT, see [LICENSE](LICENSE).

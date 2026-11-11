# Forge IDE

A lightweight, native code editor written in Rust, built on [egui](https://github.com/emilk/egui).

> **Status:** early scaffold. Expect rough edges.

## Features

- File explorer with lazily-loaded directory tree
- Tabbed editing with unsaved-change indicators
- Syntax highlighting for many languages (via [syntect](https://github.com/trishume/syntect))
- Cursor line/column in the status bar
- Native open/save dialogs (via [rfd](https://github.com/PolyMeilex/rfd))
- Language server browser (**Tools → Language Servers…**): finds servers for the
  languages in your project, shows which are installed, and installs missing ones
  with one click via the language's own toolchain (rustup, npm, go, pipx, …)

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
├── workspace.rs         # file tree / project explorer, file-type scan
├── lsp/
│   ├── catalog.rs       # known language servers and their install commands
│   ├── detect.rs        # finding installed servers / package managers
│   ├── install.rs       # background install jobs
│   └── browser.rs       # "Language Servers" window
└── editor/
    ├── document.rs      # open file buffer, load/save, dirty tracking
    └── highlight.rs     # syntect → egui LayoutJob highlighting
```

## Roadmap

- [ ] Line-number gutter
- [ ] Rope-based buffer for large files, undo/redo history
- [ ] Find / replace, go to line
- [ ] Command palette
- [ ] Unsaved-changes prompt on close
- [ ] Integrated terminal
- [x] Language server discovery and installation
- [ ] LSP client (diagnostics, completion, go to definition)
- [ ] Settings and themes

## License

MIT, see [LICENSE](LICENSE).

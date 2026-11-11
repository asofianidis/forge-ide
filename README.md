# Forge IDE

A lightweight, native code editor written in Rust, built on [egui](https://github.com/emilk/egui).

> **Status:** early scaffold. Expect rough edges.

## Features

- File explorer with lazily-loaded directory tree
- Tabbed editing with unsaved-change indicators
- Rope-backed buffers with grouped undo/redo history
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
| Close tab  | Ctrl+W   |
| Undo       | Ctrl+Z   |
| Redo       | Ctrl+Y / Ctrl+Shift+Z |

(Cmd instead of Ctrl on macOS.)

## Project layout

```
src/
├── main.rs              # entry point, window setup
├── app.rs               # application state and UI layout
├── workspace.rs         # file tree / project explorer
└── editor/
    ├── buffer.rs        # rope-backed text buffer, undo/redo history
    ├── document.rs      # open file buffer, load/save, dirty tracking
    ├── gutter.rs        # line-number gutter
    └── highlight.rs     # syntect → egui LayoutJob highlighting
```

## Roadmap

- [x] Line-number gutter
- [x] Rope-based buffer for large files, undo/redo history
- [ ] Find / replace, go to line
- [ ] Command palette
- [ ] Unsaved-changes prompt on close
- [ ] Integrated terminal
- [ ] LSP client (diagnostics, completion, go to definition)
- [ ] Settings and themes

## License

MIT, see [LICENSE](LICENSE).

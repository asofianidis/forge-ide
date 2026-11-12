# Forge IDE

A lightweight, native code editor written in Rust, built on [egui](https://github.com/emilk/egui).

> **Status:** early scaffold. Expect rough edges.

## Features

- Activity bar with five sidebar views: **Explorer**, **Search**, **Source Control**, **Pull Requests**
  and **Extensions**
- File explorer with lazily-loaded directory tree, colored by git status. Right-click to create
  files and folders (`a/b/c.rs` creates the folders in between), rename, delete (to the trash),
  reveal in Finder / File Explorer, or copy the path
- File watching (via [notify](https://github.com/notify-rs/notify)): the explorer, open files and
  git status follow changes made by other programs as they happen. Unmodified open files reload;
  files with unsaved edits show a banner to reload or keep your version
- Tabbed editing with unsaved-change indicators
- Rope-backed buffers with grouped undo/redo history. Every edit is undoable, including line
  commands, replace, hunk revert, conflict resolution, language-server edits and reloads from
  disk; undoing back to the saved state clears the unsaved marker.
  *Limitation:* egui's `TextEdit` needs the text as one contiguous string, so the buffer keeps a
  `String` mirror for rendering and layout is still O(file size). Truly large files need a custom
  editor widget that lays out only the visible lines; the buffer is the model for that.
- Session restore: each folder reopens with the tabs, selected tab and cursor positions it had
  last time. Starting Forge without a folder argument opens the last folder you used
  (`forge-ide .` opens the current directory)
- **File → Open Recent** (or Ctrl+R) lists the last 10 folders you opened
- Syntax highlighting for many languages (via [syntect](https://github.com/trishume/syntect))
- Markdown preview (Cmd/Ctrl+Shift+V, the eye button at the end of the tab bar, or
  **View → Markdown Preview**): renders the open `.md` file beside the editor and follows your
  edits as you type. Web links open in the browser; relative links (`docs/guide.md`,
  `/src/main.rs`) open the file in the editor
- Line-number gutter with current-line highlight
- Minimap (**View → Minimap**): a zoomed-out, syntax-colored view of the file beside the editor
  with the visible part outlined. Click or drag it to scroll; the mouse wheel over it scrolls too
- [EditorConfig](https://editorconfig.org) support (see below)
- Customizable keyboard shortcuts in `settings.toml` (see below)
- Cursor line/column in the status bar
- Find and replace in the current file, with match case, whole word and regex options
  (regex replacements can use `$1` / `${name}` capture groups)
- Project-wide search (**View → Search**) built on ripgrep's `ignore` and `grep-searcher`
  crates, so `.gitignore`d, hidden and binary files are skipped
- Quick open (fuzzy file search) and a command palette that reaches every menu action
- Line editing: go to line, toggle comment, indent/outdent a selection (Tab / Shift+Tab
  work on multi-line selections), duplicate and move lines, matching-bracket highlighting
- Typing aids: Enter keeps the line's indentation (one level deeper after an opening bracket, or
  `:` in Python/YAML); brackets and quotes auto-close, wrap a selection, and step over their
  closer; Backspace in an empty pair deletes both halves
- Native open/save dialogs (via [rfd](https://github.com/PolyMeilex/rfd))
- Language server browser (**Tools → Language Servers…**): finds servers for the
  languages in your project, shows which are installed, and installs missing ones
  with one click via the language's own toolchain (rustup, npm, go, pipx, …)
- Language server client: installed servers start automatically for open files (see below)
- Git source control (see below) with live change markers in the editor gutter
- GitHub pull requests through the GitHub CLI (see below)
- Integrated terminal (Ctrl+\`): your shell in a pseudo-terminal below the editor, with colors
  and scrollback (see below)
- Themes: built-in **Blossom** (default) and **Blossom Light**, plus custom TOML themes
- Settings window (Ctrl+,): color theme, font size, word wrap, line numbers, minimap, format on save,
  update checks. Changes apply and save immediately; the file can also be edited as TOML
- Extensions installed from `.zip` files: commands that run any program with the open file or
  selection (palette, shortcuts, on save) and color themes (see below)
- Automatic updates from GitHub releases

## EditorConfig

Forge reads `.editorconfig` files from the edited file's folder upward, stopping at one with
`root = true`. Closer files win, and later sections win within a file. Supported properties:

- `indent_style`, `indent_size`, `tab_width`: what Tab, Enter, indent/outdent and language server
  formatting use. Anything left unset falls back to the indentation the file already uses
- `end_of_line` (`lf` or `crlf`), `trim_trailing_whitespace`, `insert_final_newline`: applied
  when saving, as one undoable edit

Globs support `*`, `**`, `?`, `[abc]`, `[!abc]`, `{a,b}` and `{1..9}`. Changes to `.editorconfig`
files apply to open tabs right away. Not supported: `end_of_line = cr`, `charset`, and drawing tabs
`tab_width` wide (the editor always draws a tab four spaces wide).

## Keyboard shortcuts

**File → Keyboard Shortcuts…** lists every command with its id and current shortcuts. To change one,
add the command id to `[keybindings]` in `settings.toml`:

```toml
[keybindings]
toggle_terminal = "cmd+j"                          # replaces the default
duplicate_line = ["cmd+shift+d", "alt+shift+down"] # several shortcuts
go_to_line = []                                    # no shortcut
```

`cmd` means Cmd on macOS and Ctrl elsewhere; `ctrl`, `alt` (or `option`) and `shift` mean the same
key everywhere. Keys are letters, digits, `f1`–`f35`, names like `up`, `pagedown`, `esc`, `enter`,
`space`, or punctuation (`cmd+,`, `` ctrl+` ``, `cmd++`). A shortcut you assign is taken away from any
command that had it by default. Mistakes (an unknown command or key, or one shortcut given to two
commands) are reported in the status bar and the Keyboard Shortcuts window, and that command keeps
its defaults. Changes apply when the settings file is saved.

## Language servers

When a file has an installed language server (see **Tools → Language Servers…**), Forge starts
it for the open folder and keeps it in sync as you type. The status bar shows the server and what
it is doing (e.g. indexing progress).

- **Diagnostics**: errors, warnings and hints are underlined in the editor; the status bar counts
  errors and warnings across the project. Click the counts (or Cmd/Ctrl+Shift+M) for a list of
  every problem, and use F8 / Shift+F8 to step through the current file's
- **Hover**: rest the pointer on a symbol for its documentation (and any diagnostics there)
- **Go to definition**: F12, or Cmd/Ctrl+click a symbol
- **Completion** as you type, after trigger characters like `.`, or with Ctrl+Space. Up/Down to
  choose, Enter or Tab to accept, Escape to dismiss. Snippets and auto-imports are applied
- **Rename symbol** (F2) across files, **Find all references** (Shift+F12)
- **Format document** (Cmd/Ctrl+Shift+I), and **Tools → Format on Save**
- **Tools → Restart Language Servers** if a server gets stuck or crashes

## Source control

The **Source Control** view (Cmd/Ctrl+Shift+G) drives the `git` command line, so it works with
your existing config, hooks, credentials and SSH keys:

- Staged, unstaged and merge-conflict files; click one to open its diff as a tab
- Diffs with syntax highlighting, word-level highlights of what changed, and a **Unified / Split**
  (side-by-side) toggle
- Stage, unstage or discard whole files, **individual hunks**, or **selected lines** (click changed
  lines in a diff, Shift+click for a range)
- Commit (Cmd/Ctrl+Enter), Commit All when nothing is staged, Commit & Push, amend, undo last commit
- Branch picker: switch, create, track remote branches; merge and delete from the Branches list
- Push / pull / fetch / sync with ahead-behind counts; publish new branches; force push with lease
- Stashes (stash, apply, pop, drop), commit history with full commit diffs
- **Commit graph** (View → Repository History, or the History section's graph button): branch
  lanes, merges, branch and tag labels; filter by author, message or all branches. Right-click a
  commit to cherry-pick, revert, create a branch or tag there, check it out, or start an
  interactive rebase from it
- **File history** (right-click a file or folder in the explorer → Show History, or View → File
  History)
- **Interactive rebase** (⋯ → Interactive Rebase…): reorder commits and pick, reword, edit, squash,
  fixup or drop each one; no text editor involved. Paused rebases, merges, cherry-picks and reverts
  show a banner with Continue / Skip / Abort
- **Tags**: create (lightweight or annotated) at HEAD or any commit, push one or all, delete locally
  and on the remote
- Commit message guide (subject ≤ 50 characters, body lines ≤ 72); amending prefills the previous
  message and shows what will change
- Merge conflicts: conflicted files open with the current and incoming blocks highlighted and
  **Accept Current / Incoming / Both** actions on each conflict (or for the whole file), then
  **Save & Mark Resolved**; also accept ours/theirs for a whole file and abort merge
- Explorer colors and editor gutter markers for added, modified and deleted lines

In the editor:

- **Click a gutter marker** to peek at the change: see the lines it replaced, **Revert** it in the
  buffer or **Stage** just that change. Alt+F5 / Shift+Alt+F5 jump to the next / previous change.
- **Blame** (View → Blame, Cmd/Ctrl+Alt+B) shows who last changed each line and when, including
  unsaved edits. Hover for the commit message; click to open the commit.

The current branch and its pull request are also shown at the left of the status bar.

## Pull requests

The **Pull Requests** view (Cmd/Ctrl+Shift+R) uses the [GitHub CLI](https://cli.github.com), which
handles sign-in and finds the GitHub repository from the folder's remotes. Install it and run
`gh auth login` once; Forge never stores tokens.

- List open, merged, closed or all PRs; filter by created by me, review requested or assigned, or
  any GitHub search query. The PR for your current branch is pinned to the top.
- Open a PR as a tab: description, comments and reviews, checks, reviewers, labels
- **Files changed** with the full diff and inline review threads; click any line to comment,
  reply to threads
- Review changes: comment, approve or request changes. Inline comments can be posted right away
  or collected into a **pending review** and submitted together with your verdict
- **Suggested changes**: insert a suggestion for a line, and apply others' suggestions to your
  working copy when the PR branch is checked out
- Resolve / unresolve comment threads; mark files as **viewed**
- Edit the title, description, labels, assignees and milestone
- **CI**: check status for the current commit in the status bar, failed-job logs in a tab, and
  re-run failed jobs (also from the PR's checks list)
- **Inbox**: unread review requests, comments and mentions on this repository's PRs, with a badge
  on the Pull Requests icon
- Merge (squash, merge commit or rebase), update branch, ready for review / convert to draft,
  close or reopen, request reviewers, check out the PR locally
- Create a pull request from the current branch (pushing it first if needed)

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

## Settings

Open **File → Settings…** (Ctrl+,) to change the color theme, editor font size, word wrap,
line numbers, format on save and update checks. Changes apply and are saved right away.

Settings live in `settings.toml` in the config folder (`~/.config/forge-ide/` on Linux,
`~/Library/Application Support/forge-ide/` on macOS, `%APPDATA%\forge-ide\` on Windows).
Set `FORGE_SETTINGS` to use a different file. **Edit as TOML** in the Settings window (or
**File → Open Settings File**) opens it in a tab, and saving that tab applies it. Missing
keys use their defaults, and the font size is kept between 8 and 32.

```toml
theme = "Blossom"       # any built-in or custom theme name
font_size = 14.0
word_wrap = false
line_numbers = true
format_on_save = false
auto_update = true      # check for updates on startup (default: on for release builds)
```

The same file also remembers recent folders and each folder's open tabs.

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

## Terminal

Ctrl+\` (Ctrl on macOS too), **Terminal → Toggle Terminal** or the command palette opens a
resizable panel below the editor running your shell in the open folder. It defaults to `$SHELL`
(PowerShell on Windows); set `terminal_shell` in `settings.toml` to use another, with arguments:

```toml
terminal_shell = "bash --login"
```

`settings.toml` lives next to the themes folder (e.g. `~/.config/forge-ide/` on Linux); a change
applies the next time the shell starts. Output
supports 16/256/true colors, bold/italic/underline/inverse and wide characters; scroll back with
the mouse wheel. Ctrl+C reaches the shell. When the shell exits the panel shows its exit code;
press Enter, click the restart button or run **Terminal: Restart Shell** to start a new one.
Ctrl+\` while the terminal has focus hides it (the shell keeps running).

## Extensions

The **Extensions** view (Cmd/Ctrl+Shift+X) lists installed extensions. Install one with **+**
(or **Tools → Extensions → Install Extension from ZIP…**, or by dropping a `.zip` onto the
window); Forge shows what it adds and which programs it runs before installing. Each extension
can be disabled, uninstalled or shown in detail, and its commands run from the view, the command
palette, **Tools → Extensions**, or their own shortcuts.

An extension is a folder with an `extension.toml` manifest. It can add:

- **Commands** that run a program (any language) with the file, selection or document and put
  the output in the status bar, an output tab, the selection, or the whole document. Commands can
  ask for input, apply only to some file types, and run automatically on save.
- **Color themes**.

Extensions live in `extensions/` in the config folder (or `$FORGE_EXTENSIONS_DIR`). See
[`docs/extensions.md`](docs/extensions.md) to write one (also under **Tools → Extensions →
Extension Authoring Guide**), and [`examples/extensions/text-tools`](examples/extensions/text-tools)
for a complete example.

## Getting started

Requires a recent stable Rust toolchain (edition 2024).

```sh
cargo run --release              # opens the current directory
cargo run --release -- path/to/project
```

### Keyboard shortcuts

| Action                 | Shortcut                                |
|------------------------|-----------------------------------------|
| New file               | Ctrl+N                                  |
| Open file              | Ctrl+O                                  |
| Save                   | Ctrl+S                                  |
| Save as                | Ctrl+Shift+S                            |
| Close tab              | Ctrl+W                                  |
| Undo                   | Ctrl+Z                                  |
| Redo                   | Ctrl+Y / Ctrl+Shift+Z (Cmd+Shift+Z on macOS) |
| Next / previous tab    | Ctrl+Tab / Ctrl+Shift+Tab (Ctrl on macOS too) |
| Command palette        | Ctrl+Shift+P                            |
| Go to file             | Ctrl+P                                  |
| Go to line             | Ctrl+G (Ctrl on macOS too), `line:col` works |
| Find                   | Ctrl+F (Enter / Shift+Enter, F3 / Shift+F3 step through matches) |
| Replace                | Ctrl+H (Cmd+Option+F on macOS)          |
| Find in project        | Ctrl+Shift+F                            |
| Show explorer          | Ctrl+Shift+E                            |
| Source control         | Ctrl+Shift+G                            |
| Pull requests          | Ctrl+Shift+R                            |
| Extensions             | Ctrl+Shift+X                            |
| Toggle blame           | Ctrl+Alt+B                              |
| Next / previous change | Alt+F5 / Shift+Alt+F5                   |
| Toggle line comment    | Ctrl+/                                  |
| Indent / outdent       | Ctrl+] / Ctrl+[ (or Tab / Shift+Tab)    |
| Move line up / down    | Alt+Up / Alt+Down                       |
| Duplicate line         | Shift+Alt+Down                          |
| Go to definition       | F12 (or Ctrl+click)                     |
| Find all references    | Shift+F12                               |
| Rename symbol          | F2                                      |
| Trigger completion     | Ctrl+Space (Ctrl on macOS too)          |
| Format document        | Ctrl+Shift+I                            |
| Problems               | Ctrl+Shift+M                            |
| Next / prev. problem   | F8 / Shift+F8                           |
| Toggle terminal        | Ctrl+\` (Ctrl on macOS too)             |
| Settings               | Ctrl+,                                  |
| Toggle word wrap       | Alt+Z                                   |
| Font size bigger / smaller / reset | Ctrl+= / Ctrl+- / Ctrl+0    |

(Cmd instead of Ctrl on macOS, except for Go to line and switching tabs.) macOS uses Cmd+H to hide the app, so
Replace is also on Cmd+Option+F there. Every action is also in the command palette.

## Project layout

```
src/
├── main.rs              # entry point, window setup
├── app.rs               # application state and UI layout
├── app/
│   ├── editing.rs       # line-editing commands, selection helpers, Go to Line
│   ├── explorer.rs      # file tree, its new / rename / delete / reveal actions, watcher events
│   ├── language.rs      # diagnostics, hover, completion, definition, rename, formatting UI
│   ├── find_bar.rs      # find / replace bar
│   ├── markdown_preview.rs # Markdown preview beside the editor, link resolution
│   ├── search_panel.rs  # "Search" sidebar
│   ├── extension_ui.rs  # Extensions view, running extension commands, install dialog
│   ├── preferences.rs   # Settings and Keyboard Shortcuts windows, font size / wrap / minimap
│   ├── pickers.rs       # quick open, command palette, theme picker
│   └── terminal_panel.rs # terminal panel: show / hide, restart
├── commands.rs          # every action: name, menu, default shortcut (menus + palette use it)
├── keymap.rs            # shortcuts in effect: defaults + [keybindings] from settings.toml
├── editorconfig.rs      # .editorconfig parsing, glob matching, save rules
├── palette.rs           # list picker used by quick open and the command palette
├── fuzzy.rs             # fuzzy matcher for file and command names
├── search.rs            # project file listing and text search (ignore + grep-searcher)
├── panes.rs             # diff / commit / pull request / log tabs
├── history.rs           # commit graph tab and interactive rebase tab
├── cli.rs               # running git and gh
├── task.rs              # background work helper
├── terminal.rs          # integrated terminal (PTY + vt100 screen, key encoding)
├── settings.rs          # persisted preferences (settings.toml)
├── theme.rs             # built-in + custom themes
├── updater.rs           # self-update from GitHub releases
├── watcher.rs           # file watching (notify), batching events for the UI
├── workspace.rs         # file tree / project explorer, file operations, file-type scan
├── extensions/
│   ├── mod.rs           # loading installed extensions, command lookup, shortcuts
│   ├── manifest.rs      # extension.toml format, variables, keybinding parsing
│   ├── install.rs       # installing from .zip, uninstalling
│   ├── run.rs           # running a command as a process with a time limit
│   └── panel.rs         # Extensions sidebar
├── git/
│   ├── mod.rs           # repository state and operations
│   ├── status.rs        # `git status --porcelain=v2` parser
│   ├── diff.rs          # unified diff parser, single-hunk patches
│   ├── line_changes.rs  # gutter change markers and change hunks
│   ├── blame.rs         # `git blame --porcelain` parser
│   ├── graph.rs         # commit graph lane layout
│   └── panel.rs         # Source Control sidebar
├── github/
│   ├── mod.rs           # gh connection, PR list, creating PRs
│   ├── model.rs         # gh JSON types
│   ├── list.rs          # Pull Requests sidebar
│   └── pr.rs            # pull request tab
├── ui/
│   ├── activity.rs      # activity bar
│   ├── diff_view.rs     # diff rendering (unified / split, line selection)
│   ├── diff_style.rs    # diff syntax highlighting and word-level changes
│   ├── markdown.rs      # Markdown for PR bodies, comments and the preview
│   ├── icons.rs         # vector icons
│   └── fmt.rs           # relative times
├── lsp/
│   ├── catalog.rs       # known language servers and their install commands
│   ├── detect.rs        # finding installed servers / package managers
│   ├── install.rs       # background install jobs
│   ├── rpc.rs           # JSON-RPC framing over stdio
│   ├── client.rs        # one server process: threads, initialize handshake
│   ├── session.rs       # servers per workspace, document sync, requests → editor events
│   ├── text.rs          # positions/encodings, URIs, snippets, applying edits
│   └── browser.rs       # "Language Servers" window
└── editor/
    ├── buffer.rs        # rope-backed text buffer, undo/redo history
    ├── document.rs      # open file buffer, load/save, dirty tracking
    ├── find.rs          # in-file find / replace
    ├── ops.rs           # comment, indent, move lines, auto-indent/pairs, brackets
    ├── minimap.rs       # zoomed-out file overview beside the editor
    ├── gutter.rs        # line-number gutter and change markers
    ├── highlight.rs     # syntect → egui LayoutJob highlighting
    ├── scm.rs           # change peek, blame column, conflict actions
    └── conflicts.rs     # merge conflict parsing and resolution
```

## Roadmap

- [x] Line-number gutter
- [x] Rope-based buffer for large files, undo/redo history
- [x] Find / replace, go to line
- [x] Project search, quick open
- [x] Command palette
- [x] Unsaved-changes prompt on close
- [x] Integrated terminal
- [x] Language server discovery and installation
- [x] LSP client: diagnostics, hover, go to definition, completion, rename, references, formatting
- [x] Themes
- [x] Git source control and GitHub pull requests
- [x] Settings UI
- [x] Extensions (commands and themes, installed from .zip)
- [x] `.editorconfig` support
- [x] Customizable keybindings
- [x] Minimap

## License

MIT, see [LICENSE](LICENSE).

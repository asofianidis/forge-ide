# Forge IDE

A lightweight, native code editor written in Rust, built on [egui](https://github.com/emilk/egui).

> **Status:** early scaffold. Expect rough edges.

## Features

- Activity bar with three sidebar views: **Explorer**, **Source Control** and **Pull Requests**
- File explorer with lazily-loaded directory tree, colored by git status
- Tabbed editing with unsaved-change indicators
- Syntax highlighting for many languages (via [syntect](https://github.com/trishume/syntect))
- Line-number gutter with current-line highlight
- Cursor line/column in the status bar
- Native open/save dialogs (via [rfd](https://github.com/PolyMeilex/rfd))
- Language server browser (**Tools → Language Servers…**): finds servers for the
  languages in your project, shows which are installed, and installs missing ones
  with one click via the language's own toolchain (rustup, npm, go, pipx, …)
- Git source control (see below) with live change markers in the editor gutter
- GitHub pull requests through the GitHub CLI (see below)
- Themes: built-in **Blossom** (default) and **Blossom Light**, plus custom TOML themes
- Automatic updates from GitHub releases

## Source control

The **Source Control** view (Cmd/Ctrl+Shift+G) drives the `git` command line, so it works with
your existing config, hooks, credentials and SSH keys:

- Staged, unstaged and merge-conflict files; click one to open its diff as a tab
- Stage, unstage or discard whole files, or **individual hunks** from the diff view
- Commit (Cmd/Ctrl+Enter), Commit All when nothing is staged, Commit & Push, amend, undo last commit
- Branch picker: switch, create, track remote branches; merge and delete from the Branches list
- Push / pull / fetch / sync with ahead-behind counts; publish new branches; force push with lease
- Stashes (stash, apply, pop, drop), commit history with full commit diffs
- Conflict resolution: accept ours/theirs, mark resolved, abort merge
- Explorer colors and editor gutter markers for added, modified and deleted lines

The current branch and its pull request are also shown at the left of the status bar.

## Pull requests

The **Pull Requests** view (Cmd/Ctrl+Shift+P) uses the [GitHub CLI](https://cli.github.com), which
handles sign-in and finds the GitHub repository from the folder's remotes. Install it and run
`gh auth login` once; Forge never stores tokens.

- List open, merged, closed or all PRs; filter by created by me, review requested or assigned, or
  any GitHub search query. The PR for your current branch is pinned to the top.
- Open a PR as a tab: description, comments and reviews, checks, reviewers, labels
- **Files changed** with the full diff and inline review threads; click any line to comment,
  reply to threads
- Review changes: comment, approve or request changes
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
| Explorer   | Ctrl+Shift+E |
| Source Control | Ctrl+Shift+G |
| Pull Requests  | Ctrl+Shift+P |

(Cmd instead of Ctrl on macOS.)

## Project layout

```
src/
├── main.rs              # entry point, window setup
├── app.rs               # application state and UI layout
├── panes.rs             # diff / commit / pull request tabs
├── cli.rs               # running git and gh
├── task.rs              # background work helper
├── settings.rs          # persisted preferences (settings.toml)
├── theme.rs             # built-in + custom themes
├── updater.rs           # self-update from GitHub releases
├── workspace.rs         # file tree / project explorer, file-type scan
├── git/
│   ├── mod.rs           # repository state and operations
│   ├── status.rs        # `git status --porcelain=v2` parser
│   ├── diff.rs          # unified diff parser, single-hunk patches
│   ├── line_changes.rs  # gutter change markers
│   └── panel.rs         # Source Control sidebar
├── github/
│   ├── mod.rs           # gh connection, PR list, creating PRs
│   ├── model.rs         # gh JSON types
│   ├── list.rs          # Pull Requests sidebar
│   └── pr.rs            # pull request tab
├── ui/
│   ├── activity.rs      # activity bar
│   ├── diff_view.rs     # diff rendering
│   ├── markdown.rs      # Markdown for PR bodies and comments
│   ├── icons.rs         # vector icons
│   └── fmt.rs           # relative times
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
- [x] Git source control and GitHub pull requests
- [ ] Settings UI

## License

MIT, see [LICENSE](LICENSE).

# Writing Forge IDE extensions

An extension is a folder with an `extension.toml` manifest. It can add:

- **Commands**: run any program (a Python script, a shell script, a compiled
  tool, `npx something`) with details about the open file, and put its output
  in the status bar, an output tab, the selection, or the whole document.
  Commands appear in the command palette, the Extensions view and
  **Tools → Extensions**; they can have a keyboard shortcut and can run
  automatically when a file is saved.
- **Color themes**, in the same TOML format as custom themes.

Extensions run as separate processes, so they can be written in any language
and can't crash or freeze the editor. You share them as `.zip` files.

This guide is written for people and for coding agents. Everything an
extension can do is listed here; if a field isn't in this document, Forge
rejects it.

## Quick start

1. Make a folder with two files:

   `hello/extension.toml`

   ```toml
   id = "hello"
   name = "Hello"
   version = "0.1.0"
   description = "Greets the open file."

   [[commands]]
   id = "greet"
   title = "Say Hello"
   program = "python3"
   args = ["${extensionPath}/hello.py", "${fileBasename}"]
   windows = { program = "python" }
   keybinding = "Cmd+Alt+J"
   ```

   `hello/hello.py`

   ```python
   import sys
   print(f"Hello from {sys.argv[1]}!")
   ```

2. Zip the folder: `cd hello && zip -r ../hello.zip .`
   (zipping the folder itself, `zip -r hello.zip hello`, also works).
3. In Forge, open the **Extensions** view (the blocks icon in the activity bar,
   or Cmd/Ctrl+Shift+X), click **+** and pick `hello.zip`. Dropping the zip
   onto the window works too.
4. Open a saved file and press Cmd+Alt+J (Ctrl+Alt+J on Windows and Linux), or
   run **Hello: Say Hello** from the command palette. The status bar shows
   `Hello from main.rs!`.

A complete example with every output kind is in
[`examples/extensions/text-tools`](../examples/extensions/text-tools) in the
Forge repository.

## Where extensions live

Installing copies the extension to `extensions/<id>/` in Forge's config folder:

| OS      | Location                                              |
|---------|-------------------------------------------------------|
| Linux   | `~/.config/forge-ide/extensions/`                     |
| macOS   | `~/Library/Application Support/forge-ide/extensions/` |
| Windows | `%APPDATA%\forge-ide\extensions\`                     |

Set the `FORGE_EXTENSIONS_DIR` environment variable to use another folder.
Every folder in it with an `extension.toml` is an extension; folders starting
with `.` are ignored.

## The manifest

`extension.toml` sits at the top of the extension folder.

```toml
id = "text-tools"            # required
name = "Text Tools"          # required
version = "1.0.0"            # required
description = "Sort lines, count words and more."
author = "Your Name"
homepage = "https://example.com/text-tools"
min_forge_version = "0.1.0"  # refuse to load on older Forge versions
themes = ["themes/paper.toml"]

[[commands]]
# ... one table per command, see below
```

| Field               | Required | Meaning |
|---------------------|----------|---------|
| `id`                | yes      | Unique identifier and install folder name. 1–64 characters: lowercase letters, digits, `-`, `_`, `.`; starts with a letter or digit. Never change it between versions; installing a zip with the same `id` replaces the installed copy. |
| `name`              | yes      | Display name, also the prefix of its commands in the palette. |
| `version`           | yes      | Any non-empty string; `MAJOR.MINOR.PATCH` is recommended. |
| `description`       | no       | One or two sentences, shown in the Extensions view. |
| `author`            | no       | Shown as "by …". |
| `homepage`          | no       | Link shown in the details tab. |
| `min_forge_version` | no       | e.g. `"0.2.0"`. Older Forge versions show an error instead of loading the extension. |
| `themes`            | no       | Theme files, relative to the extension folder. |
| `commands`          | no       | Array of command tables (`[[commands]]`). |

Unknown fields are errors, so typos are caught when the extension loads. A
`README.md` next to the manifest is shown when the user clicks **Details**.

## Commands

```toml
[[commands]]
id = "sort-lines"                 # required, unique in this extension
title = "Sort Lines"              # required
program = "python3"               # required
args = ["${extensionPath}/tools.py", "sort"]
windows = { program = "python" }  # per-OS override of program and/or args
stdin = "selection"               # none (default) | selection | document
output = "replace-selection"      # see "Output"; default "status"
keybinding = "Cmd+Alt+O"
file_types = ["txt", "md"]        # default: every file
prompt = "Sort by which column?"  # ask for ${input} first
on_save = false                   # run after a matching file is saved
cwd = "${workspaceFolder}"        # default: see below
timeout = 60                      # seconds, 0 = no limit; default 60
```

| Field        | Default            | Meaning |
|--------------|--------------------|---------|
| `id`         | (required)         | Same rules as the extension id. The command's full id is `<extension id>:<command id>`, e.g. `text-tools:sort-lines`. |
| `title`      | (required)         | Shown in the palette as `<extension name>: <title>`. |
| `program`    | (required)         | What to run. See "Finding the program". |
| `args`       | `[]`               | Arguments. Each is passed as-is: no shell, no quoting, no globbing. Variables are expanded. |
| `windows`, `macos`, `linux` | none | `{ program = "...", args = [...] }`; either key replaces the default on that OS. |
| `cwd`        | see below          | Working directory; variables allowed. A relative path is relative to the open folder. |
| `stdin`      | `"none"`           | `"selection"`: the selected text (empty if nothing is selected). `"document"`: the whole text of the active tab, including unsaved edits. |
| `output`     | `"status"`         | What to do with stdout. See "Output". |
| `keybinding` | none               | See "Keyboard shortcuts". |
| `file_types` | `[]` (all files)   | Extensions (`"rs"`, `".rs"` and `"RS"` all work) or exact file names (`"Dockerfile"`). `"*"` matches any file. With a list, the command only shows in the palette for matching files, and it can't run in an untitled tab. |
| `prompt`     | none               | Text of a question asked before running; the answer is `${input}`. |
| `on_save`    | `false`            | Run after a matching file is saved. See "Running on save". |
| `timeout`    | `60`               | Seconds before the program is stopped (and the run reported as failed). `0` disables the limit. |

The default working directory is the open folder; without one, the folder of
the active file; without that, the extension folder.

### Finding the program

- A bare name (`python3`, `node`, `rg`, `npx`) is looked up on `PATH`.
- A path with a `/` or `\` (`bin/tool`, `./run.sh`) is relative to the
  extension folder. Executable bits are kept when installing from a zip, so
  `program = "./run.sh"` works on macOS and Linux if the script is executable
  and has a `#!` line.
- `${extensionPath}` also works: `program = "${extensionPath}/bin/tool"`.
- An absolute path is used as is.

There is no shell. To use pipes or shell syntax, run the shell yourself:
`program = "sh"`, `args = ["-c", "sort | uniq"]` (and on Windows
`windows = { program = "powershell", args = ["-NoProfile", "-Command", "..."] }`).
Interpreted scripts are the most portable choice: `python3` (`python` on
Windows) or `node`.

### Variables

Use `${name}` in `program`, `args` and `cwd`. Write `$$` for a literal `$`.
An unknown variable is an error when the extension loads.

| Variable                     | Value |
|------------------------------|-------|
| `${file}`                    | Absolute path of the active file |
| `${fileBasename}`            | `main.rs` |
| `${fileBasenameNoExtension}` | `main` |
| `${fileExtname}`             | `.rs` (empty if none) |
| `${fileDirname}`             | Folder of the active file |
| `${relativeFile}`            | Path relative to the open folder (`src/main.rs`) |
| `${workspaceFolder}`         | The open folder |
| `${extensionPath}`           | This extension's install folder |
| `${selectedText}`            | The selection (prefer `stdin = "selection"` for long text) |
| `${lineNumber}`, `${column}` | 1-based caret position |
| `${input}`                   | The answer to `prompt` (empty without a prompt) |

The file variables need the active tab to be a saved file, and
`${workspaceFolder}` needs an open folder; otherwise the command isn't run and
the status bar says why. The file variables refer to the file on disk, which
doesn't include unsaved edits: use `stdin = "document"` for the current text.

The same information is in environment variables, which is handy for scripts:

| Environment variable   | Set when |
|------------------------|----------|
| `FORGE_VERSION`        | always |
| `FORGE_EXTENSION_PATH` | always |
| `FORGE_FILE`           | the active tab is a saved file |
| `FORGE_RELATIVE_FILE`  | the active tab is a saved file |
| `FORGE_WORKSPACE`      | a folder is open |
| `FORGE_LINE`, `FORGE_COLUMN` | always (1 and 1 without a file) |
| `FORGE_INPUT`          | the command has a `prompt` |

### Output

stdout is read as UTF-8. Exit with status 0 for success.

| `output`              | On success |
|-----------------------|------------|
| `"status"` (default)  | The first non-empty line of stdout is shown in the status bar ("<title>: done" if there is none). |
| `"panel"`             | stdout and stderr open in an output tab (reused by the next run). Good for reports and long output. |
| `"replace-selection"` | stdout replaces the selected text, which stays selected. With no selection it is inserted at the caret. |
| `"insert"`            | stdout is inserted at the caret, and the caret moves after it. |
| `"replace-document"`  | stdout replaces the whole text of the tab (formatters). |
| `"new-document"`      | stdout opens in a new untitled tab. |
| `"silent"`            | Nothing. |

Rules for the edit outputs (`replace-selection`, `insert`, `replace-document`):

- They apply to the tab the command ran on, as one undo step.
- If that tab's text changed while the program ran, the result is discarded
  (the status bar says so) rather than overwriting the user's typing.
- For `replace-selection` and `insert`, one trailing newline in stdout is
  removed unless the replaced text ended with a newline. `print()` in most
  languages adds one; this keeps it from adding a blank line.
- `replace-document` with output identical to the text changes nothing.
- These commands need an editor tab; their shortcuts only work while the
  editor has focus.

**Errors.** A non-zero exit status, a timeout, or a program that can't be
started counts as a failure. The status bar shows "Error: <title>:" followed
by the last non-empty line of stderr (else of stdout, else the exit status).
Edit outputs are not applied on failure. With `output = "panel"` the output
tab opens anyway. For any command, the **Show last output** button next to it
in the Extensions view shows the last run's full stdout and stderr: use it to
debug.

So the convention for scripts is: print the result to stdout; on failure,
print a short, user-facing message to stderr and exit non-zero (in Python,
`sys.exit("Select some lines first")` does both).

### Keyboard shortcuts

`keybinding` is modifiers and a key joined with `+`, case-insensitive:

- `Cmd` (or `Mod`): Command on macOS, Ctrl on Windows and Linux. Use this
  for the usual "primary" modifier.
- `Ctrl`: Ctrl on every platform.
- `Alt` (or `Option`), `Shift`.
- Keys: `A`–`Z`, `0`–`9`, `F1`–`F20`, `Enter`, `Space`, `Tab`, `Escape`,
  `Backspace`, `Delete`, `Up`/`Down`/`Left`/`Right`, `Home`, `End`, `PageUp`,
  `PageDown`, and punctuation such as `Comma`, `Period`, `Slash`, `Minus`,
  `Plus` (or the characters themselves, except `+`: use `Plus`).

Examples: `"Cmd+Shift+L"`, `"Ctrl+Alt+T"`, `"Alt+F7"`, `"F6"`.

A shortcut needs Cmd, Ctrl or Alt, unless it is a function key, so that it
never steals normal typing. A shortcut already used by Forge or by another
extension is ignored, and the Extensions view shows a warning; the command
still works from the palette. Avoid the built-in shortcuts listed in the
README, and prefer `Cmd+Alt+<letter>` or `Ctrl+Alt+<letter>` combinations.

### Prompts

With `prompt`, running the command first asks the user for a line of text. It
is available as `${input}` and `FORGE_INPUT`. Cancelling doesn't run the
command. The answer can be empty; check it in your program.

### Running on save

With `on_save = true`, the command also runs after the user saves a file that
matches `file_types` (every file if `file_types` is empty, so set it). It can
still be run by hand.

- Only `output = "status"`, `"panel"`, `"silent"` or `"replace-document"` are
  allowed, and no `prompt` or `stdin = "selection"`.
- It runs after the file is written (after format on save, if that is on), in
  the background, so saving never waits for it.
- With `replace-document`, if the output differs from the text, Forge applies
  it and saves the file again (without running the on-save commands again).
  If the user typed in the meantime, the result is discarded.
- `"status"` output that prints nothing shows nothing. Prefer `"silent"` for
  checks that only matter when they fail, so saving stays quiet.
- Several on-save commands for one file run at the same time. If two of them
  rewrite the document, the second sees a changed document and is discarded.

## Themes

List theme files in `themes`. They use the format of custom themes (see
`themes/example.toml` in the Forge repository): only `name` is required, and
everything else is inherited from the theme named in `extends`.

```toml
name = "Paper"
extends = "Blossom Light"

[ui]
accent = "#2f6f62"
editor = "#fbf9f4"
```

Extension themes show up in **View → Theme** and as **Theme: <name>** links in
the Extensions view. A theme in the user's own themes folder with the same
name takes precedence. `extends` can name a built-in theme (`"Blossom"`,
`"Blossom Light"`), not another extension's theme.

## Packaging

A package is a `.zip` file with `extension.toml` either at the top level or
inside a single top-level folder:

```
text-tools.zip                 text-tools.zip
├── extension.toml             └── text-tools/
├── tools.py                       ├── extension.toml
├── README.md                      ├── tools.py
└── themes/paper.toml              └── ...
```

```sh
# from inside the extension folder
zip -r ../text-tools.zip . -x '.*'
# or, on Windows (PowerShell)
Compress-Archive -Path * -DestinationPath ..\text-tools.zip
```

Rules checked before anything is installed:

- Exactly one `extension.toml` at the top or one folder down, and it must be
  valid (the same checks as when loading).
- Every file in `themes` must be in the zip.
- No paths that point outside the folder (`../`, absolute paths).
- At most 10,000 files and 256 MB unpacked.
- `__MACOSX/`, `.DS_Store` and `Thumbs.db` are skipped.

On macOS and Linux the files' permission bits are kept (minus setuid and
world-writable bits), so executables stay executable. Use `zip` rather than
tools that drop permissions if you ship scripts run directly as `program`.

## Installing, updating and removing

- **Install**: Extensions view → **+**, **Tools → Extensions → Install
  Extension from ZIP…**, the command palette, or drop a `.zip` onto the
  window. Forge shows what the extension adds and which programs it runs, and
  asks for confirmation.
- **Update**: install a zip with the same `id`. The old copy is replaced
  completely (files not in the new zip are removed), only after the new one
  unpacks and loads correctly.
- **Disable / enable**: the checkbox on the extension's card. Disabled
  extensions keep their files; their commands, shortcuts and themes go away.
  The setting is `disabled_extensions` in `settings.toml`.
- **Uninstall**: **Uninstall** on the card (click twice to confirm). This
  deletes the extension's folder.

## Developing an extension

You don't need to re-zip on every change:

1. Install your extension once, or create its folder directly in the
   extensions folder (**Tools → Extensions → Open Extensions Folder**).
2. Edit the files there (or symlink the folder to your working copy on macOS
   and Linux).
3. Run **Extensions: Reload Extensions** (or the reload button in the
   Extensions view). Manifest errors are shown on the extension's card.

Scripts are started fresh for every run, so script changes apply on the next
run without reloading; manifest changes need a reload.

Test your program outside Forge the way Forge runs it, e.g.
`printf 'b\na\n' | python3 tools.py sort`. When a run fails inside Forge, use
**Show last output** next to the command in the Extensions view.

During development you can point Forge at a scratch folder with
`FORGE_EXTENSIONS_DIR=/path/to/folder forge-ide`.

## Security

Extensions run programs with the user's permissions, like scripts in a
terminal. Forge doesn't sandbox them. The install dialog lists the programs an
extension runs and its on-save commands; users should only install extensions
from people they trust. Extensions should:

- do only what their title says, and never change files other than through
  their `output`;
- not send the user's code anywhere without saying so in the description;
- not need network access for editing commands.

## Checklist for agents

When asked to create a Forge extension:

1. Pick a lowercase `id`; make the folder name match it.
2. Write `extension.toml` with `id`, `name`, `version`, `description`, and one
   `[[commands]]` table per action. Use only the fields in this document.
3. Prefer a single script (`python3`, with `windows = { program = "python" }`,
   or `node`) invoked as `"${extensionPath}/script.py"` with a subcommand
   argument. Read input from stdin, write the result to stdout, write errors
   to stderr and exit non-zero.
4. Choose `stdin` and `output` together: transforming text →
   `stdin = "selection"` + `output = "replace-selection"`; whole-file
   formatting → `stdin = "document"` + `output = "replace-document"`;
   reports → `output = "panel"`; short answers → `output = "status"`.
5. Set `file_types` for language-specific commands, and always for `on_save`.
6. Only add a `keybinding` if asked; use `Cmd+Alt+<letter>` and avoid
   Forge's built-in shortcuts.
7. Add a `README.md` describing the commands and requirements (e.g. "Requires
   Python 3").
8. Check the script by running it in a terminal with sample stdin, then zip
   the folder (`zip -r ../<id>.zip .`).

## Not supported (yet)

Extensions can't add sidebar views, editor decorations, language servers,
snippets, settings, or menus other than the automatic **Tools → Extensions**
list, and commands can't talk back to the editor while running (no streaming
output or follow-up requests). Long-running watchers or servers should be
started from the integrated terminal instead.

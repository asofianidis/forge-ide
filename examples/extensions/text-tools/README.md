# Text Tools

An example extension for Forge IDE. It shows every kind of output an extension
command can produce:

| Command                  | Input      | Output                         |
|--------------------------|------------|--------------------------------|
| Sort Lines               | selection  | replaces the selection         |
| Count Words              | document   | status bar                     |
| Wrap Selection in Tag…   | selection  | replaces the selection (asks for a tag first) |
| Insert Timestamp         | none       | inserted at the caret          |
| List TODOs in Folder     | none       | output tab                     |
| Trim Trailing Whitespace | document   | rewrites `.md` / `.txt` files on save |

It also adds the **Paper** color theme.

Requires Python 3 (`python3` on macOS and Linux, `python` on Windows).

## Install

```sh
cd examples/extensions/text-tools
zip -r ../text-tools.zip .
```

Then in Forge: Extensions view → **+** (Install from ZIP…) → pick `text-tools.zip`.

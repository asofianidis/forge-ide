"""Text Tools for Forge IDE: one script, a subcommand per extension command.

Input comes on stdin (see `stdin` in extension.toml); whatever is printed to
stdout is the result. Exit with a non-zero status and a message on stderr to
report an error in Forge's status bar.
"""

import datetime
import os
import re
import sys


def read_stdin():
    # Forge sends UTF-8 text; keep line endings exactly as they are.
    return sys.stdin.buffer.read().decode("utf-8")


def write(text):
    sys.stdout.buffer.write(text.encode("utf-8"))


def sort_lines():
    text = read_stdin()
    if not text:
        sys.exit("Select some lines to sort")
    ends_with_newline = text.endswith("\n")
    lines = sorted(text.splitlines(), key=str.casefold)
    write("\n".join(lines) + ("\n" if ends_with_newline else ""))


def count():
    text = read_stdin()
    words = len(text.split())
    lines = text.count("\n") + (1 if text and not text.endswith("\n") else 0)
    print(f"{words} words, {len(text)} characters, {lines} lines")


def wrap(tag):
    tag = tag.strip()
    if not re.fullmatch(r"[A-Za-z][\w:-]*", tag):
        sys.exit(f"Not a tag name: {tag!r}")
    write(f"<{tag}>{read_stdin()}</{tag}>")


def timestamp():
    write(datetime.datetime.now().astimezone().isoformat(timespec="seconds"))


SKIP_DIRS = {".git", "node_modules", "target", "dist", "build", "__pycache__", ".venv"}


def todos(folder):
    pattern = re.compile(r"\b(TODO|FIXME|HACK|XXX)\b[:\s]*(.*)")
    found = 0
    for root, dirs, files in os.walk(folder):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS and not d.startswith("."))
        for name in sorted(files):
            path = os.path.join(root, name)
            try:
                with open(path, encoding="utf-8") as f:
                    for number, line in enumerate(f, 1):
                        m = pattern.search(line)
                        if m:
                            found += 1
                            rel = os.path.relpath(path, folder)
                            print(f"{rel}:{number}: {m.group(1)} {m.group(2).strip()}")
            except (UnicodeDecodeError, OSError):
                continue  # binary or unreadable file
    print(f"\n{found} found")


def trim():
    text = read_stdin()
    write(re.sub(r"[ \t]+(?=\r?\n|$)", "", text))


def main():
    if len(sys.argv) < 2:
        sys.exit("usage: tools.py <command> [args]")
    command, args = sys.argv[1], sys.argv[2:]
    if command == "sort":
        sort_lines()
    elif command == "count":
        count()
    elif command == "wrap":
        wrap(args[0] if args else "")
    elif command == "timestamp":
        timestamp()
    elif command == "todos":
        todos(args[0] if args else ".")
    elif command == "trim":
        trim()
    else:
        sys.exit(f"unknown command {command!r}")


if __name__ == "__main__":
    main()

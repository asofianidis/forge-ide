//! Parsing unified diffs as produced by `git diff` and `gh pr diff`, and
//! turning single hunks back into patches for `git apply`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number in the old file (context and removed lines).
    pub old_no: Option<u32>,
    /// Line number in the new file (context and added lines).
    pub new_no: Option<u32>,
    /// Line content without the leading `+`/`-`/space.
    pub text: String,
    /// Followed by `\ No newline at end of file`.
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The full `@@ -a,b +c,d @@ context` line.
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// The text after the second `@@`, usually the enclosing function.
    pub fn section(&self) -> &str {
        self.header
            .splitn(3, "@@")
            .nth(2)
            .map(str::trim)
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    /// Raw header lines (`diff --git` through `+++`), used to build patches.
    pub header: Vec<String>,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// The path to show: the new path, or the old one for deletions.
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }

    pub fn additions(&self) -> usize {
        self.count(LineKind::Added)
    }

    pub fn deletions(&self) -> usize {
        self.count(LineKind::Removed)
    }

    fn count(&self, kind: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == kind)
            .count()
    }

    /// A patch containing only hunk `idx`, suitable for `git apply`.
    pub fn hunk_patch(&self, idx: usize) -> Option<String> {
        let hunk = self.hunks.get(idx)?;
        let mut out = String::new();
        for line in &self.header {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&hunk.header);
        out.push('\n');
        for line in &hunk.lines {
            out.push(match line.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
            });
            out.push_str(&line.text);
            out.push('\n');
            if line.no_newline {
                out.push_str("\\ No newline at end of file\n");
            }
        }
        Some(out)
    }
}

/// Parses a (possibly multi-file) unified diff. Text before the first
/// `diff --git` line (e.g. a commit header) is ignored.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    // Lines still expected in the current hunk: (old, new).
    let mut remaining = (0u32, 0u32);
    let mut old_no = 0;
    let mut new_no = 0;

    for line in text.split('\n') {
        let in_hunk = remaining.0 > 0 || remaining.1 > 0;
        if in_hunk && let Some(file) = files.last_mut() {
            let hunk = file.hunks.last_mut().expect("in_hunk implies a hunk");
            // Some tools strip the space from empty context lines.
            let (prefix, rest) = if line.is_empty() {
                (' ', "")
            } else {
                let mut chars = line.chars();
                let p = chars.next().unwrap_or(' ');
                (p, chars.as_str())
            };
            let (kind, o, n) = match prefix {
                '+' => (LineKind::Added, None, Some(new_no)),
                '-' => (LineKind::Removed, Some(old_no), None),
                '\\' => {
                    if let Some(last) = hunk.lines.last_mut() {
                        last.no_newline = true;
                    }
                    continue;
                }
                _ => (LineKind::Context, Some(old_no), Some(new_no)),
            };
            if o.is_some() {
                old_no += 1;
                remaining.0 = remaining.0.saturating_sub(1);
            }
            if n.is_some() {
                new_no += 1;
                remaining.1 = remaining.1.saturating_sub(1);
            }
            hunk.lines.push(DiffLine {
                kind,
                old_no: o,
                new_no: n,
                text: rest.to_owned(),
                no_newline: false,
            });
            continue;
        }
        // A trailing "\ No newline" right after a hunk that just completed.
        if line.starts_with('\\') {
            if let Some(last) = files
                .last_mut()
                .and_then(|f| f.hunks.last_mut())
                .and_then(|h| h.lines.last_mut())
            {
                last.no_newline = true;
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("diff --git ") {
            let (old, new) = split_git_paths(rest);
            files.push(FileDiff {
                old_path: old,
                new_path: new,
                status: FileStatus::Modified,
                binary: false,
                header: vec![line.to_owned()],
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if let Some(h) = parse_hunk_header(line) {
            old_no = h.old_start;
            new_no = h.new_start;
            remaining = (h.old_count, h.new_count);
            file.hunks.push(h);
            continue;
        }
        if !file.hunks.is_empty() {
            // Stray text after the last hunk.
            continue;
        }
        file.header.push(line.to_owned());
        if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
            file.old_path = None;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
            file.new_path = None;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(p.to_owned());
        } else if let Some(p) = line.strip_prefix("rename to ") {
            file.new_path = Some(p.to_owned());
        } else if let Some(p) = line.strip_prefix("copy from ") {
            file.status = FileStatus::Copied;
            file.old_path = Some(p.to_owned());
        } else if let Some(p) = line.strip_prefix("copy to ") {
            file.new_path = Some(p.to_owned());
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            file.binary = true;
        } else if let Some(p) = line.strip_prefix("--- ") {
            file.old_path = strip_side(p, "a/");
        } else if let Some(p) = line.strip_prefix("+++ ") {
            file.new_path = strip_side(p, "b/");
        }
    }
    files
}

/// `/dev/null` means the file doesn't exist on that side.
fn strip_side(path: &str, prefix: &str) -> Option<String> {
    let path = path.trim_end_matches('\t');
    if path == "/dev/null" {
        None
    } else {
        Some(path.strip_prefix(prefix).unwrap_or(path).to_owned())
    }
}

/// Best-effort split of `a/old b/new` (ambiguous when paths contain " b/";
/// the `---`/`+++` lines, when present, override this).
fn split_git_paths(rest: &str) -> (Option<String>, Option<String>) {
    let old = rest.strip_prefix("a/").unwrap_or(rest);
    match old.find(" b/") {
        Some(i) => (Some(old[..i].to_owned()), Some(old[i + 3..].to_owned())),
        None => (Some(old.to_owned()), Some(old.to_owned())),
    }
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |r: &str| -> Option<(u32, u32)> {
        match r.split_once(',') {
            Some((s, c)) => Some((s.parse().ok()?, c.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Some(Hunk {
        header: line.to_owned(),
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main() {
 use std::io;
-fn old() {}
+fn new() {}
+fn extra() {}

 fn tail() {}
@@ -10,2 +11,2 @@
 a
-b
+c
\\ No newline at end of file
diff --git a/new file.txt b/new file.txt
new file mode 100644
index 0000000..333
--- /dev/null
+++ b/new file.txt
@@ -0,0 +1 @@
+hello
diff --git a/old.rs b/renamed.rs
similarity index 90%
rename from old.rs
rename to renamed.rs
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
";

    #[test]
    fn parses_files_and_hunks() {
        let files = parse(SAMPLE);
        assert_eq!(files.len(), 4);

        let main = &files[0];
        assert_eq!(main.path(), "src/main.rs");
        assert_eq!(main.status, FileStatus::Modified);
        assert_eq!(main.hunks.len(), 2);
        assert_eq!(main.additions(), 3);
        assert_eq!(main.deletions(), 2);
        let h = &main.hunks[0];
        assert_eq!(h.section(), "fn main() {");
        assert_eq!(h.lines.len(), 6);
        // The empty line was a context line whose space was stripped.
        assert_eq!(h.lines[4].kind, LineKind::Context);
        assert_eq!(h.lines[4].old_no, Some(3));
        assert_eq!(h.lines[4].new_no, Some(4));
        assert_eq!(h.lines[5].new_no, Some(5));
        assert!(main.hunks[1].lines.last().unwrap().no_newline);

        let added = &files[1];
        assert_eq!(added.status, FileStatus::Added);
        assert_eq!(added.path(), "new file.txt");
        assert_eq!(added.old_path, None);
        assert_eq!(added.hunks[0].lines[0].new_no, Some(1));

        let renamed = &files[2];
        assert_eq!(renamed.status, FileStatus::Renamed);
        assert_eq!(renamed.old_path.as_deref(), Some("old.rs"));
        assert_eq!(renamed.path(), "renamed.rs");
        assert!(renamed.hunks.is_empty());

        assert!(files[3].binary);
    }

    #[test]
    fn hunk_patch_round_trips() {
        let files = parse(SAMPLE);
        let patch = files[0].hunk_patch(1).unwrap();
        assert_eq!(
            patch,
            "diff --git a/src/main.rs b/src/main.rs\nindex 111..222 100644\n\
             --- a/src/main.rs\n+++ b/src/main.rs\n@@ -10,2 +11,2 @@\n a\n-b\n+c\n\
             \\ No newline at end of file\n"
        );
        let reparsed = parse(&patch);
        assert_eq!(reparsed[0].hunks, vec![files[0].hunks[1].clone()]);
    }
}

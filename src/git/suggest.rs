//! Rule-based commit messages and pull request text, worked out from the
//! diff itself: which files changed, how, and which definitions (functions,
//! types, classes) were added, removed or edited. No network or model.

use std::collections::BTreeSet;

use super::diff::{FileDiff, FileStatus, LineKind};

const SUBJECT_SOFT: usize = 50;
const SUBJECT_HARD: usize = 72;
const WRAP: usize = 72;
const MAX_BODY_FILES: usize = 30;
const MAX_NAMES: usize = 4;

// ---- per-file summary -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verb {
    Add,
    Update,
    Remove,
    Rename,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Add => "Add",
            Verb::Update => "Update",
            Verb::Remove => "Remove",
            Verb::Rename => "Rename",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Code,
    Docs,
    Tests,
    Ci,
    Deps,
}

/// What happened to one file.
#[derive(Debug, Clone)]
struct FileSummary {
    path: String,
    old_path: Option<String>,
    verb: Verb,
    category: Category,
    binary: bool,
    additions: usize,
    deletions: usize,
    /// Definitions that are new in this file.
    added: Vec<String>,
    /// Definitions that no longer exist.
    removed: Vec<String>,
    /// Definitions whose bodies or signatures changed.
    changed: Vec<String>,
}

impl FileSummary {
    fn new(f: &FileDiff) -> Self {
        let path = f.path().to_owned();
        let verb = match f.status {
            FileStatus::Added | FileStatus::Copied => Verb::Add,
            FileStatus::Deleted => Verb::Remove,
            FileStatus::Renamed => Verb::Rename,
            FileStatus::Modified => Verb::Update,
        };
        let mut added_defs = BTreeSet::new();
        let mut removed_defs = BTreeSet::new();
        let mut sections = Vec::new();
        for hunk in &f.hunks {
            // The definition enclosing each line: the hunk header's, then
            // whichever definition was last seen above it.
            let mut enclosing = definition(hunk.section());
            for line in &hunk.lines {
                let def = definition(&line.text);
                match (line.kind, def) {
                    (LineKind::Added, Some(name)) => {
                        added_defs.insert(name.clone());
                        enclosing = Some(name);
                    }
                    (LineKind::Removed, Some(name)) => {
                        removed_defs.insert(name);
                    }
                    (LineKind::Context, Some(name)) => enclosing = Some(name),
                    (LineKind::Context, None) => {}
                    (_, None) => {
                        if !is_annotation(&line.text)
                            && let Some(name) = &enclosing
                            && !sections.contains(name)
                        {
                            sections.push(name.clone());
                        }
                    }
                }
            }
        }
        let added: Vec<String> = added_defs.difference(&removed_defs).cloned().collect();
        let removed: Vec<String> = removed_defs.difference(&added_defs).cloned().collect();
        let mut changed: Vec<String> = Vec::new();
        for name in added_defs
            .intersection(&removed_defs)
            .cloned()
            .chain(sections)
        {
            if !added.contains(&name) && !removed.contains(&name) && !changed.contains(&name) {
                changed.push(name);
            }
        }
        // A new or deleted file is described by the file itself.
        let (added, removed, changed) = match verb {
            Verb::Add => (added, Vec::new(), Vec::new()),
            Verb::Remove => (Vec::new(), Vec::new(), Vec::new()),
            _ => (added, removed, changed),
        };
        Self {
            category: category(&path),
            old_path: f.old_path.clone().filter(|o| *o != path),
            path,
            verb,
            binary: f.binary,
            additions: f.additions(),
            deletions: f.deletions(),
            added,
            removed,
            changed,
        }
    }

    fn name(&self) -> &str {
        file_name(&self.path)
    }

    /// "add foo, bar; update baz" for the body.
    fn details(&self) -> String {
        match self.verb {
            Verb::Add => {
                if self.added.is_empty() {
                    "new file".to_owned()
                } else {
                    format!("new file with {}", name_list(&self.added))
                }
            }
            Verb::Remove => "deleted".to_owned(),
            Verb::Rename | Verb::Update => {
                let mut parts = Vec::new();
                if let Some(old) = &self.old_path {
                    parts.push(format!("renamed from {old}"));
                }
                for (word, names) in [
                    ("add", &self.added),
                    ("update", &self.changed),
                    ("remove", &self.removed),
                ] {
                    if !names.is_empty() {
                        parts.push(format!("{word} {}", name_list(names)));
                    }
                }
                parts.join("; ")
            }
        }
    }

    fn stats(&self) -> String {
        if self.binary {
            "binary".to_owned()
        } else {
            format!("+{} −{}", self.additions, self.deletions)
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn category(path: &str) -> Category {
    let lower = path.to_ascii_lowercase();
    let name = file_name(&lower);
    let in_dir = |d: &str| lower.starts_with(&format!("{d}/")) || lower.contains(&format!("/{d}/"));
    if lower.starts_with(".github/workflows/")
        || lower.starts_with(".circleci/")
        || name == ".gitlab-ci.yml"
        || name == "azure-pipelines.yml"
        || name == "jenkinsfile"
    {
        return Category::Ci;
    }
    const LOCKS: &[&str] = &[
        "cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lockb",
        "poetry.lock",
        "pipfile.lock",
        "gemfile.lock",
        "go.sum",
        "composer.lock",
        "uv.lock",
    ];
    if LOCKS.contains(&name) {
        return Category::Deps;
    }
    if in_dir("tests")
        || in_dir("test")
        || in_dir("__tests__")
        || in_dir("spec")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains("_test.")
        || name.starts_with("test_")
    {
        return Category::Tests;
    }
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    if matches!(ext, "md" | "mdx" | "rst" | "adoc" | "txt")
        || in_dir("docs")
        || name == "license"
        || name.starts_with("license.")
    {
        return Category::Docs;
    }
    Category::Code
}

/// The name declared on a line of source code, if it declares a function,
/// type or class in a common language.
pub fn definition(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    // Skip modifiers such as `pub(crate) async unsafe` or `export default`.
    const MODIFIERS: &[&str] = &[
        "pub(crate)",
        "pub(super)",
        "pub(self)",
        "pub",
        "export",
        "default",
        "async",
        "unsafe",
        "extern \"C\"",
        "public",
        "private",
        "protected",
        "static",
        "abstract",
        "final",
        "override",
        "open",
        "internal",
        "inline",
    ];
    'strip: loop {
        // Rust's `const fn` (but not `const x = …`).
        if let Some(r) = rest.strip_prefix("const fn ") {
            rest = &rest[rest.len() - r.len() - 3..];
            continue;
        }
        for m in MODIFIERS {
            if let Some(r) = rest.strip_prefix(m)
                && (r.starts_with(char::is_whitespace) || m.ends_with(')'))
            {
                rest = r.trim_start();
                continue 'strip;
            }
        }
        break;
    }
    let keyword_end = rest.find(|c: char| !c.is_ascii_alphabetic())?;
    let (keyword, after) = rest.split_at(keyword_end);
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let mut after = after.trim_start();
    match keyword {
        "fn" | "struct" | "enum" | "trait" | "union" | "def" | "class" | "function"
        | "interface" | "fun" | "func" | "macro_rules" => {}
        // `const handler = (…) =>` and `const f = function`.
        "const" | "let" | "var" => {
            let name = identifier(after)?;
            let tail = after[name.len()..]
                .trim_start()
                .strip_prefix('=')?
                .trim_start();
            let is_fn = tail.starts_with("function")
                || tail.starts_with("async")
                || (tail.starts_with('(') && tail.contains("=>"));
            return is_fn.then_some(name);
        }
        _ => return None,
    }
    if keyword == "macro_rules" {
        after = after.strip_prefix('!')?.trim_start();
    }
    // Go methods: `func (r *Repo) Name(`.
    if keyword == "func" && after.starts_with('(') {
        after = after[after.find(')')? + 1..].trim_start();
    }
    // `function* gen` in JavaScript.
    after = after.trim_start_matches('*').trim_start();
    identifier(after)
}

/// Blank lines, comments and attributes, which usually belong to the
/// definition below them rather than the one above.
fn is_annotation(line: &str) -> bool {
    let t = line.trim_start();
    t.is_empty()
        || [
            "//", "/*", "*", "#[", "#!", "#", "@", "--", "\"\"\"", "<!--",
        ]
        .iter()
        .any(|p| t.starts_with(p))
}

fn identifier(s: &str) -> Option<String> {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(s.len());
    let id = &s[..end];
    (!id.is_empty() && !id.starts_with(|c: char| c.is_ascii_digit())).then(|| id.to_owned())
}

/// "a, b and c", or "a, b and 3 more".
fn name_list(names: &[String]) -> String {
    let shown: Vec<&str> = names.iter().take(MAX_NAMES).map(String::as_str).collect();
    let extra = names.len().saturating_sub(MAX_NAMES);
    if extra > 0 {
        return format!("{} and {extra} more", shown.join(", "));
    }
    join_and(&shown)
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The deepest folder every path shares ("" when none).
fn common_dir(paths: &[&str]) -> String {
    let mut iter = paths.iter().map(|p| dir_of(p));
    let Some(first) = iter.next() else {
        return String::new();
    };
    let mut common: Vec<&str> = first.split('/').filter(|s| !s.is_empty()).collect();
    for d in iter {
        let parts: Vec<&str> = d.split('/').filter(|s| !s.is_empty()).collect();
        let n = common
            .iter()
            .zip(&parts)
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(n);
    }
    common.join("/")
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Picks the most descriptive candidate that fits the soft limit, then the
/// hard limit, then the shortest.
fn pick(candidates: Vec<String>) -> String {
    let fits = |limit: usize| {
        candidates
            .iter()
            .find(|c| c.chars().count() <= limit)
            .cloned()
    };
    fits(SUBJECT_SOFT)
        .or_else(|| fits(SUBJECT_HARD))
        .or_else(|| candidates.iter().min_by_key(|c| c.chars().count()).cloned())
        .unwrap_or_default()
}

// ---- subjects ---------------------------------------------------------------

fn single_file_subjects(f: &FileSummary) -> Vec<String> {
    let name = f.name();
    let mut out = Vec::new();
    match f.verb {
        Verb::Add => out.push(format!("Add {name}")),
        Verb::Remove => out.push(format!("Remove {name}")),
        Verb::Rename => {
            let old = f.old_path.as_deref().unwrap_or("");
            if file_name(old) == name {
                let dir = dir_of(&f.path);
                out.push(format!(
                    "Move {name} to {}",
                    if dir.is_empty() { "the root" } else { dir }
                ));
            } else {
                out.push(format!("Rename {} to {name}", file_name(old)));
            }
        }
        Verb::Update => {
            let (a, r, c) = (&f.added, &f.removed, &f.changed);
            if !a.is_empty() && r.is_empty() && a.len() <= 2 {
                out.push(format!("Add {} to {name}", join_and(&refs(a))));
                if c.len() == 1 {
                    out.insert(
                        0,
                        format!("Add {} and update {} in {name}", join_and(&refs(a)), c[0]),
                    );
                }
            }
            if !r.is_empty() && a.is_empty() && r.len() <= 2 {
                out.push(format!("Remove {} from {name}", join_and(&refs(r))));
            }
            if a.is_empty() && r.is_empty() && !c.is_empty() && c.len() <= 2 {
                out.push(format!("Update {} in {name}", join_and(&refs(c))));
            }
            out.push(format!("Update {name}"));
        }
    }
    out
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn category_subject(files: &[FileSummary]) -> Option<String> {
    let first = files.first()?.category;
    if first == Category::Code || files.iter().any(|f| f.category != first) {
        return None;
    }
    Some(
        match first {
            Category::Docs => "Update documentation",
            Category::Tests => "Update tests",
            Category::Ci => "Update CI workflows",
            Category::Deps => "Update dependencies",
            Category::Code => return None,
        }
        .to_owned(),
    )
}

fn multi_file_subjects(files: &[FileSummary]) -> Vec<String> {
    let mut out = Vec::new();
    // Manifests changing alongside lock files are a dependency update.
    let deps_only = files.iter().all(|f| {
        f.category == Category::Deps
            || matches!(
                f.name().to_ascii_lowercase().as_str(),
                "cargo.toml"
                    | "package.json"
                    | "go.mod"
                    | "pyproject.toml"
                    | "gemfile"
                    | "composer.json"
            )
    }) && files.iter().any(|f| f.category == Category::Deps);
    if deps_only {
        out.push("Update dependencies".to_owned());
    }
    out.extend(category_subject(files));

    let mut verbs: Vec<Verb> = files.iter().map(|f| f.verb).collect();
    verbs.sort();
    verbs.dedup();
    let clauses: Vec<(Verb, Vec<&str>)> = verbs
        .iter()
        .map(|v| {
            let mut names: Vec<&str> = files
                .iter()
                .filter(|f| f.verb == *v)
                .map(FileSummary::name)
                .collect();
            names.dedup();
            (*v, names)
        })
        .collect();

    // A new file wired into existing ones: name it after the new file.
    let added: Vec<&FileSummary> = files.iter().filter(|f| f.verb == Verb::Add).collect();
    if let [one] = added.as_slice()
        && one.category == Category::Code
        && files.iter().all(|f| f.verb != Verb::Remove)
    {
        let stem = one.name().split('.').next().unwrap_or(one.name());
        out.push(format!(
            "Add {stem} and use it in {}",
            join_and(
                &files
                    .iter()
                    .filter(|f| f.verb == Verb::Update)
                    .map(FileSummary::name)
                    .collect::<Vec<_>>()
            )
        ));
    }

    // Every file named.
    let named: Vec<String> = clauses
        .iter()
        .enumerate()
        .map(|(i, (v, names))| {
            let word = if i == 0 {
                v.word().to_owned()
            } else {
                lower_first(v.word())
            };
            format!("{word} {}", join_and(names))
        })
        .collect();
    out.push(named.join("; "));

    // Counts, scoped to the shared folder.
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let dir = common_dir(&paths);
    let suffix = if dir.is_empty() {
        String::new()
    } else {
        format!(" in {dir}")
    };
    let counted: Vec<String> = clauses
        .iter()
        .enumerate()
        .map(|(i, (v, names))| {
            let word = if i == 0 {
                v.word().to_owned()
            } else {
                lower_first(v.word())
            };
            format!("{word} {}", plural(names.len(), "file"))
        })
        .collect();
    out.push(format!("{}{suffix}", counted.join(", ")));
    out.push(format!("Update {}{suffix}", plural(files.len(), "file")));
    out.push(format!("Update {}", plural(files.len(), "file")));
    out
}

fn subject_for(files: &[FileSummary]) -> String {
    match files {
        [] => String::new(),
        [one] => pick(single_file_subjects(one)),
        many => pick(multi_file_subjects(many)),
    }
}

// ---- conventional commits ----------------------------------------------------

/// True when most recent subjects look like `type(scope): subject`.
pub fn uses_conventional_commits(subjects: &[&str]) -> bool {
    let recent: Vec<&&str> = subjects
        .iter()
        .filter(|s| !s.starts_with("Merge "))
        .take(20)
        .collect();
    if recent.len() < 3 {
        return false;
    }
    let matching = recent
        .iter()
        .filter(|s| conventional_type(s).is_some())
        .count();
    matching * 2 > recent.len()
}

fn conventional_type(subject: &str) -> Option<&str> {
    let (head, _) = subject.split_once(": ")?;
    let ty = head.split(['(', '!']).next()?;
    (!ty.is_empty() && ty.len() <= 10 && ty.chars().all(|c| c.is_ascii_lowercase())).then_some(ty)
}

fn conventional_prefix(files: &[FileSummary]) -> String {
    let all = |c: Category| files.iter().all(|f| f.category == c);
    let ty = if all(Category::Docs) {
        "docs"
    } else if all(Category::Tests) {
        "test"
    } else if all(Category::Ci) {
        "ci"
    } else if files.iter().all(|f| {
        f.category == Category::Deps || f.name().ends_with(".toml") || f.name().ends_with(".json")
    }) && files.iter().any(|f| f.category == Category::Deps)
    {
        "build"
    } else if files
        .iter()
        .any(|f| f.verb == Verb::Add || !f.added.is_empty())
    {
        "feat"
    } else if files
        .iter()
        .all(|f| matches!(f.verb, Verb::Rename | Verb::Remove))
    {
        "refactor"
    } else {
        "chore"
    };
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let dir = common_dir(&paths);
    let scope = dir
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && !matches!(*s, "src" | "lib" | "app" | "tests" | "docs"));
    match scope {
        Some(s) => format!("{ty}({s}): "),
        None => format!("{ty}: "),
    }
}

// ---- bodies -----------------------------------------------------------------

/// Wraps a bullet at `WRAP` columns, indenting continuation lines.
fn wrap_bullet(text: &str) -> String {
    let mut out = String::new();
    let mut line = String::from("-");
    for word in text.split_whitespace() {
        if line.chars().count() + 1 + word.chars().count() > WRAP && line.trim() != "-" {
            out.push_str(&line);
            out.push('\n');
            line = String::from(" ");
        }
        line.push(' ');
        line.push_str(word);
    }
    out.push_str(&line);
    out
}

fn file_bullets(files: &[FileSummary]) -> Vec<String> {
    let mut lines: Vec<String> = files
        .iter()
        .take(MAX_BODY_FILES)
        .map(|f| {
            let details = f.details();
            if details.is_empty() {
                format!("{} ({})", f.path, f.stats())
            } else {
                format!("{}: {details} ({})", f.path, f.stats())
            }
        })
        .collect();
    if files.len() > MAX_BODY_FILES {
        lines.push(format!(
            "…and {} more",
            plural(files.len() - MAX_BODY_FILES, "file")
        ));
    }
    lines
}

// ---- public API -------------------------------------------------------------

/// A commit message for `files` (the diff about to be committed). Follows
/// conventional commits when `recent_subjects` do.
pub fn commit_message(files: &[FileDiff], recent_subjects: &[&str]) -> String {
    let files: Vec<FileSummary> = files.iter().map(FileSummary::new).collect();
    if files.is_empty() {
        return String::new();
    }
    let mut subject = subject_for(&files);
    if uses_conventional_commits(recent_subjects) {
        subject = format!("{}{}", conventional_prefix(&files), lower_first(&subject));
    }
    // A single file whose subject already says it all needs no body.
    let needs_body = files.len() > 1
        || files[0].added.len() + files[0].removed.len() + files[0].changed.len() > 2
        || (files[0].verb == Verb::Add && !files[0].added.is_empty());
    if !needs_body {
        return subject;
    }
    let body: Vec<String> = file_bullets(&files)
        .iter()
        .map(|b| wrap_bullet(b))
        .collect();
    format!("{subject}\n\n{}", body.join("\n"))
}

/// One commit on the branch being proposed.
#[derive(Debug, Clone)]
pub struct BranchCommit {
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrText {
    pub title: String,
    pub body: String,
}

/// Title and description for a pull request of `branch`. `commits` are
/// oldest first; `files` is the diff against the base; `template` is the
/// repository's pull request template, if any.
pub fn pr_text(
    branch: &str,
    commits: &[BranchCommit],
    files: &[FileDiff],
    template: Option<&str>,
) -> PrText {
    let summaries: Vec<FileSummary> = files.iter().map(FileSummary::new).collect();
    let meaningful: Vec<&BranchCommit> = commits.iter().filter(|c| !is_noise(&c.subject)).collect();

    let title = match meaningful.as_slice() {
        [one] => one.subject.clone(),
        _ => title_from_branch(branch)
            .or_else(|| (!summaries.is_empty()).then(|| subject_for(&summaries)))
            .or_else(|| meaningful.first().map(|c| c.subject.clone()))
            .unwrap_or_default(),
    };

    let mut body = String::new();
    body.push_str("## Summary\n\n");
    match meaningful.as_slice() {
        [] => body.push_str(&format!("{}\n", subject_for(&summaries))),
        [one] => {
            let text = one.body.trim();
            if text.is_empty() {
                body.push_str(&format!("{}\n", one.subject));
            } else {
                body.push_str(&format!("{text}\n"));
            }
        }
        many => {
            let mut seen = BTreeSet::new();
            for c in many {
                if seen.insert(c.subject.clone()) {
                    body.push_str(&format!("- {}\n", c.subject));
                }
            }
        }
    }

    if !summaries.is_empty() {
        let adds: usize = summaries.iter().map(|f| f.additions).sum();
        let dels: usize = summaries.iter().map(|f| f.deletions).sum();
        body.push_str(&format!(
            "\n## Changes\n\n{}, +{adds} −{dels}\n\n",
            plural(summaries.len(), "file")
        ));
        if summaries.len() > 15 {
            for line in dir_rollup(&summaries) {
                body.push_str(&format!("- {line}\n"));
            }
        } else {
            for f in &summaries {
                let details = f.details();
                let path = format!("`{}`", f.path);
                if details.is_empty() {
                    body.push_str(&format!("- {path} ({})\n", f.stats()));
                } else {
                    body.push_str(&format!("- {path}: {details} ({})\n", f.stats()));
                }
            }
        }
    }

    if let Some(issue) = issue_from_branch(branch) {
        body.push_str(&format!("\nCloses #{issue}\n"));
    }
    if let Some(t) = template.map(str::trim).filter(|t| !t.is_empty()) {
        body.push_str("\n---\n\n");
        body.push_str(t);
        body.push('\n');
    }
    PrText {
        title,
        body: body.trim_end().to_owned(),
    }
}

/// Fixups and merges that say nothing about the change.
fn is_noise(subject: &str) -> bool {
    subject.starts_with("fixup! ")
        || subject.starts_with("squash! ")
        || subject.starts_with("amend! ")
        || subject.starts_with("Merge branch ")
        || subject.starts_with("Merge remote-tracking branch ")
}

/// Per-folder totals for large pull requests.
fn dir_rollup(files: &[FileSummary]) -> Vec<String> {
    let mut dirs: Vec<(String, usize, usize, usize)> = Vec::new();
    for f in files {
        let dir = dir_of(&f.path);
        let key = if dir.is_empty() {
            "(root)".to_owned()
        } else {
            format!("`{dir}/`")
        };
        match dirs.iter_mut().find(|d| d.0 == key) {
            Some(d) => {
                d.1 += 1;
                d.2 += f.additions;
                d.3 += f.deletions;
            }
            None => dirs.push((key, 1, f.additions, f.deletions)),
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.2 + d.3));
    dirs.into_iter()
        .map(|(d, n, a, r)| format!("{d}: {} (+{a} −{r})", plural(n, "file")))
        .collect()
}

const ACRONYMS: &[(&str, &str)] = &[
    ("api", "API"),
    ("ci", "CI"),
    ("cli", "CLI"),
    ("css", "CSS"),
    ("db", "DB"),
    ("github", "GitHub"),
    ("gitlab", "GitLab"),
    ("html", "HTML"),
    ("http", "HTTP"),
    ("id", "ID"),
    ("ide", "IDE"),
    ("json", "JSON"),
    ("lsp", "LSP"),
    ("oauth", "OAuth"),
    ("pr", "PR"),
    ("prs", "PRs"),
    ("sql", "SQL"),
    ("ssh", "SSH"),
    ("toml", "TOML"),
    ("ui", "UI"),
    ("url", "URL"),
    ("ux", "UX"),
    ("yaml", "YAML"),
];

/// "feature/lsp-server-discovery" → "LSP server discovery";
/// "fix/123-crash-on-save" → "Fix crash on save". `None` for names that
/// don't read as words (generated or hash-like).
pub fn title_from_branch(branch: &str) -> Option<String> {
    let mut segments: Vec<&str> = branch.split('/').filter(|s| !s.is_empty()).collect();
    let last = segments.pop()?;
    let kind = segments.last().map(|s| s.to_ascii_lowercase());
    let mut words: Vec<String> = last
        .split(['-', '_', '.', ' '])
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    // Drop issue numbers like `123-` or `issue-123`.
    while words.first().is_some_and(|w| {
        w.chars().all(|c| c.is_ascii_digit())
            || matches!(w.to_ascii_lowercase().as_str(), "issue" | "gh")
    }) {
        words.remove(0);
    }
    if words.is_empty() {
        return None;
    }
    let hashy = |w: &String| {
        w.len() >= 6
            && w.chars().all(|c| c.is_ascii_hexdigit())
            && w.chars().any(|c| c.is_ascii_digit())
    };
    let wordy = words
        .iter()
        .filter(|w| w.chars().any(|c| c.is_alphabetic()) && !hashy(w))
        .count();
    if words.iter().any(hashy) || wordy * 2 <= words.len() || wordy == 0 {
        return None;
    }
    let mut words: Vec<String> = words
        .into_iter()
        .map(|w| {
            let lower = w.to_ascii_lowercase();
            ACRONYMS
                .iter()
                .find(|(k, _)| *k == lower)
                .map(|(_, v)| (*v).to_owned())
                .unwrap_or(w)
        })
        .collect();
    let verb = match kind.as_deref() {
        Some("fix" | "bugfix" | "hotfix") => Some("Fix"),
        Some("docs" | "doc") => Some("Document"),
        Some("refactor") => Some("Refactor"),
        Some("test" | "tests") => Some("Test"),
        _ => None,
    };
    if let Some(v) = verb
        && !words[0].eq_ignore_ascii_case(v)
    {
        words.insert(0, v.to_owned());
    }
    words[0] = upper_first(&words[0]);
    Some(words.join(" "))
}

/// An issue number named in the branch, e.g. `fix/123-x`, `issue-45`, `gh-7`.
pub fn issue_from_branch(branch: &str) -> Option<u64> {
    let last = branch.rsplit('/').next()?;
    let words: Vec<&str> = last.split(['-', '_']).collect();
    match words.as_slice() {
        [n, _, ..] if n.chars().all(|c| c.is_ascii_digit()) => n.parse().ok(),
        [tag, n, ..] if matches!(tag.to_ascii_lowercase().as_str(), "issue" | "gh") => {
            n.parse().ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff;

    fn parse(text: &str) -> Vec<FileDiff> {
        diff::parse(text)
    }

    const MODIFY: &str = "diff --git a/src/git/mod.rs b/src/git/mod.rs
index 1111111..2222222 100644
--- a/src/git/mod.rs
+++ b/src/git/mod.rs
@@ -10,3 +10,7 @@ impl SourceControl {
     pub fn refresh(&mut self) {
+        self.load();
+    }
+
+    pub fn load_worktrees(&self) {
     }
";

    const ADD: &str = "diff --git a/src/git/worktree.rs b/src/git/worktree.rs
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/src/git/worktree.rs
@@ -0,0 +1,3 @@
+pub struct Worktree {}
+
+pub fn parse(out: &str) {}
";

    #[test]
    fn finds_definitions() {
        assert_eq!(
            definition("pub(crate) async fn load(x: u8) {"),
            Some("load".into())
        );
        assert_eq!(
            definition("    pub struct Worktree {"),
            Some("Worktree".into())
        );
        assert_eq!(definition("const fn size() -> usize"), Some("size".into()));
        assert_eq!(definition("def handle(self):"), Some("handle".into()));
        assert_eq!(definition("export default class App {"), Some("App".into()));
        assert_eq!(
            definition("export const onSave = async (e) => {"),
            Some("onSave".into())
        );
        assert_eq!(
            definition("func (r *Repo) Status() error {"),
            Some("Status".into())
        );
        assert_eq!(definition("let x = 5;"), None);
        assert_eq!(definition("// fn not_code"), None);
        assert_eq!(definition("fnord();"), None);
    }

    #[test]
    fn single_file_subject_names_new_function() {
        let msg = commit_message(&parse(MODIFY), &[]);
        assert_eq!(msg, "Add load_worktrees and update refresh in mod.rs");
    }

    #[test]
    fn new_file_and_wiring() {
        let files = parse(&format!("{ADD}{MODIFY}"));
        let msg = commit_message(&files, &[]);
        let (subject, body) = msg.split_once("\n\n").unwrap();
        assert_eq!(subject, "Add worktree and use it in mod.rs");
        assert!(body.contains("- src/git/worktree.rs: new file with Worktree and parse (+3 −0)"));
        assert!(body.contains("- src/git/mod.rs: add load_worktrees; update refresh (+4 −0)"));
    }

    #[test]
    fn conventional_prefix_follows_history() {
        let history = ["feat: a", "fix(ui): b", "chore: c", "docs: d"];
        let msg = commit_message(&parse(ADD), &history);
        assert!(msg.starts_with("feat(git): add worktree.rs"), "{msg}");
    }

    #[test]
    fn docs_only() {
        let text = "diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
diff --git a/docs/guide.md b/docs/guide.md
--- a/docs/guide.md
+++ b/docs/guide.md
@@ -1 +1 @@
-old
+new
";
        assert!(commit_message(&parse(text), &[]).starts_with("Update documentation"));
    }

    #[test]
    fn many_files_fall_back_to_counts() {
        let mut text = String::new();
        for i in 0..8 {
            text.push_str(&format!(
                "diff --git a/src/ui/some_long_module_name_{i}.rs b/src/ui/some_long_module_name_{i}.rs\n--- a/src/ui/some_long_module_name_{i}.rs\n+++ b/src/ui/some_long_module_name_{i}.rs\n@@ -1 +1 @@\n-a\n+b\n"
            ));
        }
        let msg = commit_message(&parse(&text), &[]);
        assert!(msg.starts_with("Update 8 files in src/ui\n"), "{msg}");
        assert!(msg.lines().all(|l| l.chars().count() <= 72));
    }

    #[test]
    fn wraps_long_bullets() {
        let b = wrap_bullet(&"word ".repeat(30));
        assert!(b.lines().count() > 1);
        assert!(b.lines().all(|l| l.chars().count() <= WRAP));
        assert!(b.lines().skip(1).all(|l| l.starts_with("  ")));
    }

    #[test]
    fn branch_titles() {
        assert_eq!(
            title_from_branch("feature/lsp-server-discovery").as_deref(),
            Some("LSP server discovery")
        );
        assert_eq!(
            title_from_branch("fix/123-crash-on-save").as_deref(),
            Some("Fix crash on save")
        );
        assert_eq!(
            title_from_branch("source-control-github-pr-panel").as_deref(),
            Some("Source control GitHub PR panel")
        );
        assert_eq!(title_from_branch("t3-73f8e797-9fd707a997378fc1"), None);
        assert_eq!(issue_from_branch("fix/123-crash"), Some(123));
        assert_eq!(issue_from_branch("issue-45-thing"), Some(45));
        assert_eq!(issue_from_branch("feature/v2-ui"), None);
    }

    #[test]
    fn pr_text_from_commits() {
        let commits = vec![
            BranchCommit {
                subject: "Add worktree parsing".into(),
                body: String::new(),
            },
            BranchCommit {
                subject: "fixup! Add worktree parsing".into(),
                body: String::new(),
            },
            BranchCommit {
                subject: "Show worktrees in the sidebar".into(),
                body: String::new(),
            },
        ];
        let files = parse(&format!("{ADD}{MODIFY}"));
        let pr = pr_text(
            "feat/42-worktree-manager",
            &commits,
            &files,
            Some("## Checklist\n- [ ] Tests"),
        );
        assert_eq!(pr.title, "Worktree manager");
        assert!(pr.body.starts_with(
            "## Summary\n\n- Add worktree parsing\n- Show worktrees in the sidebar\n"
        ));
        assert!(!pr.body.contains("fixup!"));
        assert!(pr.body.contains("2 files, +7 −0"));
        assert!(
            pr.body
                .contains("- `src/git/worktree.rs`: new file with Worktree and parse (+3 −0)")
        );
        assert!(pr.body.contains("Closes #42"));
        assert!(pr.body.ends_with("## Checklist\n- [ ] Tests"));
    }

    #[test]
    fn single_commit_pr_uses_its_message() {
        let commits = vec![BranchCommit {
            subject: "Remove the focus outline".into(),
            body: "It looked odd on dark themes.".into(),
        }];
        let pr = pr_text("t3-abc1234def", &commits, &[], None);
        assert_eq!(pr.title, "Remove the focus outline");
        assert_eq!(pr.body, "## Summary\n\nIt looked odd on dark themes.");
    }
}

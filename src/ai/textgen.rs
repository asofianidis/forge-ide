//! One-shot text generation (commit messages, pull request text) through
//! an agent CLI, with tools turned off where the CLI allows it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::provider::{Provider, augmented_path};
use super::thread::ModelChoice;
use crate::lsp::detect::hide_console_window;

/// Diff text sent to the model is cut to this many bytes.
const MAX_DIFF: usize = 60_000;

/// A model that writes text, ready to run on any thread.
#[derive(Debug, Clone)]
pub struct TextGenerator {
    pub choice: ModelChoice,
    pub exe: PathBuf,
}

impl TextGenerator {
    /// Runs `prompt` and returns the model's reply.
    pub fn generate(&self, cwd: &Path, prompt: &str) -> Result<String, String> {
        let provider = self.choice.provider;
        let out_file = std::env::temp_dir().join(format!(
            "forge-ai-{}-{}.txt",
            std::process::id(),
            super::thread::now()
        ));
        let (args, stdin) = provider.text_args(
            cwd,
            &self.choice.model,
            &self.choice.effort,
            prompt,
            &out_file,
        );
        let mut cmd = Command::new(&self.exe);
        cmd.args(&args)
            .current_dir(cwd)
            .env("PATH", augmented_path(&self.exe))
            .env("NO_COLOR", "1")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", self.exe.display()))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            std::thread::spawn(move || {
                use std::io::Write as _;
                let _ = pipe.write_all(text.as_bytes());
            });
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{} failed: {e}", provider.name()))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let text = if provider == Provider::Codex {
            let t = std::fs::read_to_string(&out_file).unwrap_or_default();
            let _ = std::fs::remove_file(&out_file);
            t
        } else {
            stdout.clone()
        };
        if !out.status.success() || text.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let detail = [stderr.trim(), stdout.trim()]
                .into_iter()
                .find(|s| !s.is_empty())
                .map(|s| super::provider::first_line(s.lines().last().unwrap_or(s), 300))
                .unwrap_or_else(|| "no output".to_owned());
            return Err(format!(
                "{} couldn't write the text: {detail}",
                provider.name()
            ));
        }
        Ok(clean(&text))
    }

    pub fn commit_message(
        &self,
        cwd: &Path,
        diff: &str,
        recent_subjects: &[&str],
    ) -> Result<String, String> {
        let prompt = commit_prompt(diff, recent_subjects);
        let text = self.generate(cwd, &prompt)?;
        Ok(text.trim().to_owned())
    }

    /// Title and Markdown body for a pull request.
    pub fn pr_text(
        &self,
        cwd: &Path,
        branch: &str,
        commits: &str,
        diff: &str,
        template: Option<&str>,
    ) -> Result<(String, String), String> {
        let prompt = pr_prompt(branch, commits, diff, template);
        let text = self.generate(cwd, &prompt)?;
        split_title(&text).ok_or_else(|| "the model's reply had no title".to_owned())
    }
}

fn cut(diff: &str) -> String {
    if diff.len() <= MAX_DIFF {
        return diff.to_owned();
    }
    let mut end = MAX_DIFF;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… (diff cut off; {} more bytes)",
        &diff[..end],
        diff.len() - end
    )
}

pub fn commit_prompt(diff: &str, recent_subjects: &[&str]) -> String {
    let mut p = String::from(
        "Write a git commit message for the changes below.\n\
         - Subject line: imperative mood, at most 50 characters, no trailing period.\n\
         - If the change needs explaining, add a blank line and a body wrapped at 72 \
         characters saying what changed and why. Skip the body for small changes.\n\
         - Match the style of the repository's recent subjects (e.g. conventional \
         commit prefixes) if they have one.\n\
         Reply with only the commit message: no code fences, quotes or commentary. \
         Do not run tools or change any files.\n",
    );
    if !recent_subjects.is_empty() {
        p.push_str("\nRecent commit subjects:\n");
        for s in recent_subjects.iter().take(15) {
            p.push_str("- ");
            p.push_str(s);
            p.push('\n');
        }
    }
    p.push_str("\nDiff:\n");
    p.push_str(&cut(diff));
    p
}

pub fn pr_prompt(branch: &str, commits: &str, diff: &str, template: Option<&str>) -> String {
    let mut p = format!(
        "Write a GitHub pull request title and description for branch `{branch}`.\n\
         Reply in exactly this form: the title on the first line (under 72 characters, \
         no prefix like \"Title:\"), a blank line, then the description in Markdown: \
         a short summary of what changed and why, then the notable changes as a list. \
         No code fences around the whole reply. Do not run tools or change any files.\n"
    );
    if let Some(t) = template.filter(|t| !t.trim().is_empty()) {
        p.push_str("\nFill in the repository's pull request template:\n");
        p.push_str(t);
        p.push('\n');
    }
    if !commits.trim().is_empty() {
        p.push_str("\nCommits on the branch (oldest first):\n");
        p.push_str(commits);
        p.push('\n');
    }
    p.push_str("\nDiff against the base branch:\n");
    p.push_str(&cut(diff));
    p
}

/// Removes code fences or quotes some models wrap their reply in.
fn clean(text: &str) -> String {
    let t = text.trim();
    let t = match t.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map_or("", |(_, r)| r);
            rest.trim_end().strip_suffix("```").unwrap_or(rest)
        }
        None => t,
    };
    t.trim().to_owned()
}

/// Splits a reply into its first line (the title) and the rest.
pub fn split_title(text: &str) -> Option<(String, String)> {
    let text = clean(text);
    let mut lines = text.lines();
    let title = lines.by_ref().find(|l| !l.trim().is_empty())?;
    let title = title.trim().trim_start_matches('#').trim();
    let title = title
        .strip_prefix("Title:")
        .or_else(|| title.strip_prefix("**Title:**"))
        .unwrap_or(title)
        .trim()
        .trim_matches('*')
        .trim();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    let body = body
        .strip_prefix("Description:")
        .unwrap_or(&body)
        .trim()
        .to_owned();
    (!title.is_empty()).then(|| (title.to_owned(), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fences() {
        assert_eq!(clean("```\nFix bug\n```"), "Fix bug");
        assert_eq!(clean("```text\nFix bug\n\nBody\n```\n"), "Fix bug\n\nBody");
        assert_eq!(clean("  Plain  "), "Plain");
    }

    #[test]
    fn splits_title() {
        assert_eq!(
            split_title("Title: Add AI view\n\n## Summary\nStuff"),
            Some(("Add AI view".into(), "## Summary\nStuff".into()))
        );
        assert_eq!(
            split_title("\n# Add thing\nBody"),
            Some(("Add thing".into(), "Body".into()))
        );
        assert_eq!(split_title("  \n "), None);
    }

    #[test]
    fn long_diffs_are_cut() {
        let big = "x".repeat(MAX_DIFF + 10);
        let p = commit_prompt(&big, &["feat: a"]);
        assert!(p.contains("diff cut off; 10 more bytes"));
        assert!(p.contains("- feat: a"));
    }
}

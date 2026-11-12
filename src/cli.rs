//! Running external command-line tools (`git`, `gh`) and capturing output.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::lsp::detect::{find_executable, hide_console_window};

/// A command-line program bound to a working directory.
#[derive(Debug, Clone)]
pub struct Cli {
    program: PathBuf,
    cwd: PathBuf,
    /// Arguments placed before every call (e.g. `-c core.quotePath=false`).
    prefix: Vec<String>,
    env: Vec<(&'static str, &'static str)>,
}

/// Output of a finished command.
#[derive(Debug, Clone)]
pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// Turns a failed run into an error message (stderr, else stdout).
    pub fn into_result(self) -> Result<String, String> {
        if self.success {
            Ok(self.stdout)
        } else {
            Err(error_message(&self.stderr, &self.stdout, self.code))
        }
    }
}

fn error_message(stderr: &str, stdout: &str, code: Option<i32>) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let text = text.trim();
    if text.is_empty() {
        match code {
            Some(c) => format!("exited with status {c}"),
            None => "terminated by a signal".to_owned(),
        }
    } else {
        text.to_owned()
    }
}

impl Cli {
    /// Finds `name` on the system, falling back to the bare name so that the
    /// OS can still resolve it.
    pub fn locate(name: &str) -> Option<PathBuf> {
        find_executable(name)
    }

    pub fn new(program: PathBuf, cwd: PathBuf) -> Self {
        Self {
            program,
            cwd,
            prefix: Vec::new(),
            env: Vec::new(),
        }
    }

    pub fn with_prefix(mut self, prefix: &[&str]) -> Self {
        self.prefix = prefix.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    pub fn with_env(mut self, key: &'static str, value: &'static str) -> Self {
        self.env.push((key, value));
        self
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Runs the program and waits for it, returning its output whatever the
    /// exit status. Errors only if the program could not be started.
    pub fn output<S: AsRef<str>>(&self, args: &[S], stdin: Option<&str>) -> Result<Output, String> {
        let mut cmd = Command::new(&self.program);
        cmd.current_dir(&self.cwd)
            .args(&self.prefix)
            .args(args.iter().map(AsRef::as_ref))
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        hide_console_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not run {}: {e}", self.program.display()))?;
        if let Some(input) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            // Write on a separate thread so a chatty child can't deadlock us.
            let input = input.to_owned();
            std::thread::spawn(move || {
                let _ = pipe.write_all(input.as_bytes());
            });
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("could not wait for {}: {e}", self.program.display()))?;
        Ok(Output {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Runs the program; non-zero exit becomes an error carrying stderr.
    pub fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<String, String> {
        self.output(args, None)?.into_result()
    }

    /// Like [`Cli::run`], feeding `stdin` to the program.
    pub fn run_with_input<S: AsRef<str>>(&self, args: &[S], stdin: &str) -> Result<String, String> {
        self.output(args, Some(stdin))?.into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_prefers_stderr() {
        assert_eq!(error_message(" boom \n", "out", Some(1)), "boom");
        assert_eq!(error_message("", "out\n", Some(1)), "out");
        assert_eq!(error_message("", "", Some(2)), "exited with status 2");
    }
}

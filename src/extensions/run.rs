//! Running an extension command: expanding variables into a process
//! invocation, then running it off the UI thread with a time limit.

use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::manifest::{self, CommandSpec, Input};
use crate::lsp::detect::{find_executable, hide_console_window};

/// Most output kept from a command's stdout or stderr.
const MAX_OUTPUT: u64 = 32 * 1024 * 1024;

/// Editor state a command can refer to through variables.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub file: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub extension_dir: PathBuf,
    pub selection: String,
    pub document: String,
    /// 1-based caret position.
    pub line: usize,
    pub column: usize,
    /// Text the user typed into the command's prompt.
    pub input: Option<String>,
}

impl Context {
    fn variable(&self, name: &str) -> Option<String> {
        let file = self.file.as_deref();
        let path = |p: &Path| p.to_string_lossy().into_owned();
        match name {
            "file" => file.map(path),
            "fileBasename" => file?.file_name().map(|n| n.to_string_lossy().into_owned()),
            "fileBasenameNoExtension" => {
                file?.file_stem().map(|n| n.to_string_lossy().into_owned())
            }
            "fileExtname" => Some(
                file?
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default(),
            ),
            "fileDirname" => file?.parent().map(path),
            "relativeFile" => {
                let file = file?;
                let rel = self
                    .workspace
                    .as_deref()
                    .and_then(|w| file.strip_prefix(w).ok())
                    .unwrap_or(file);
                Some(path(rel))
            }
            "workspaceFolder" => self.workspace.as_deref().map(path),
            "extensionPath" => Some(path(&self.extension_dir)),
            "selectedText" => Some(self.selection.clone()),
            "lineNumber" => Some(self.line.to_string()),
            "column" => Some(self.column.to_string()),
            "input" => Some(self.input.clone().unwrap_or_default()),
            _ => None,
        }
    }

    fn expand(&self, template: &str) -> Result<String, String> {
        manifest::expand(template, |name| self.variable(name))
    }
}

/// A fully resolved process to start.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
    pub timeout: Option<Duration>,
}

impl Invocation {
    /// Expands `spec` against `ctx`. Fails on a variable without a value
    /// (e.g. `${file}` in an untitled tab).
    pub fn new(spec: &CommandSpec, ctx: &Context) -> Result<Self, String> {
        let (program, args) = spec.invocation();
        let program = ctx.expand(program)?;
        let program = resolve_program(&program, &ctx.extension_dir);
        let args = args
            .iter()
            .map(|a| ctx.expand(a))
            .collect::<Result<Vec<_>, _>>()?;
        let cwd = match &spec.cwd {
            Some(c) => {
                let c = PathBuf::from(ctx.expand(c)?);
                if c.is_absolute() {
                    c
                } else {
                    ctx.workspace
                        .as_deref()
                        .unwrap_or(&ctx.extension_dir)
                        .join(c)
                }
            }
            None => ctx
                .workspace
                .clone()
                .or_else(|| ctx.file.as_deref()?.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| ctx.extension_dir.clone()),
        };
        let stdin = match spec.stdin {
            Input::None => None,
            Input::Selection => Some(ctx.selection.clone()),
            Input::Document => Some(ctx.document.clone()),
        };
        let mut env = vec![
            (
                "FORGE_VERSION".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
            ),
            (
                "FORGE_EXTENSION_PATH".to_owned(),
                ctx.variable("extensionPath").unwrap_or_default(),
            ),
            ("FORGE_LINE".to_owned(), ctx.line.to_string()),
            ("FORGE_COLUMN".to_owned(), ctx.column.to_string()),
        ];
        for (key, var) in [
            ("FORGE_FILE", "file"),
            ("FORGE_RELATIVE_FILE", "relativeFile"),
            ("FORGE_WORKSPACE", "workspaceFolder"),
            ("FORGE_INPUT", "input"),
        ] {
            if let Some(v) = ctx
                .variable(var)
                .filter(|_| var != "input" || ctx.input.is_some())
            {
                env.push((key.to_owned(), v));
            }
        }
        Ok(Self {
            program,
            args,
            cwd,
            env,
            stdin,
            timeout: (spec.timeout > 0).then(|| Duration::from_secs(spec.timeout)),
        })
    }
}

/// A program containing a path separator is relative to the extension
/// folder; a bare name is looked up on `PATH`.
fn resolve_program(program: &str, extension_dir: &Path) -> PathBuf {
    let path = Path::new(program);
    if path.is_absolute() {
        path.to_path_buf()
    } else if program.contains('/') || program.contains('\\') {
        extension_dir.join(path)
    } else {
        find_executable(program).unwrap_or_else(|| PathBuf::from(program))
    }
}

/// What a finished command produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration: Duration,
}

impl RunOutput {
    /// A one-line explanation of a failure.
    pub fn error_summary(&self, timeout: Option<Duration>) -> String {
        if self.timed_out {
            return format!("timed out after {}s", timeout.map_or(0, |t| t.as_secs()));
        }
        let last_line = |s: &str| {
            s.lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_owned)
        };
        last_line(&self.stderr)
            .or_else(|| last_line(&self.stdout))
            .unwrap_or_else(|| match self.code {
                Some(c) => format!("exited with status {c}"),
                None => "terminated by a signal".to_owned(),
            })
    }

    /// stdout followed by stderr, for the output tab.
    pub fn combined(&self) -> String {
        let mut text = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&self.stderr);
        }
        if !self.success {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&match (self.timed_out, self.code) {
                (true, _) => "[timed out]".to_owned(),
                (_, Some(c)) => format!("[exited with status {c}]"),
                (_, None) => "[terminated by a signal]".to_owned(),
            });
        }
        text
    }
}

fn read_all(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut pipe).take(MAX_OUTPUT).read_to_end(&mut buf);
        // Keep draining so the child never blocks on a full pipe.
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        let _ = tx.send(buf);
    });
    rx
}

/// Starts the process and waits for it (or the time limit). Errors only if
/// it could not be started.
pub fn run(inv: &Invocation) -> Result<RunOutput, String> {
    let started = Instant::now();
    let mut cmd = Command::new(&inv.program);
    cmd.args(&inv.args)
        .current_dir(&inv.cwd)
        .envs(inv.env.iter().map(|(k, v)| (k, v)))
        .stdin(if inv.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console_window(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", inv.program.display()))?;
    if let (Some(input), Some(mut pipe)) = (inv.stdin.clone(), child.stdin.take()) {
        // Write on a separate thread so a chatty child can't deadlock us.
        std::thread::spawn(move || {
            let _ = pipe.write_all(input.as_bytes());
        });
    }
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => return Err(format!("could not wait for {}: {e}", inv.program.display())),
        }
        if inv.timeout.is_some_and(|t| started.elapsed() > t) {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // A background process the command started may hold the pipes open;
    // don't wait on it forever.
    let collect = |rx: Option<mpsc::Receiver<Vec<u8>>>| {
        rx.and_then(|rx| rx.recv_timeout(Duration::from_secs(2)).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    Ok(RunOutput {
        success: !timed_out && status.is_some_and(|s| s.success()),
        code: status.and_then(|s| s.code()),
        stdout: collect(stdout),
        stderr: collect(stderr),
        timed_out,
        duration: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::manifest::Manifest;

    fn spec(cmd: &str) -> CommandSpec {
        let src = format!("id = \"t\"\nname = \"T\"\nversion = \"1\"\n[[commands]]\n{cmd}");
        Manifest::parse(&src).unwrap().commands.remove(0)
    }

    fn ctx() -> Context {
        Context {
            file: Some(PathBuf::from("/work/src/main.rs")),
            workspace: Some(PathBuf::from("/work")),
            extension_dir: PathBuf::from("/ext/t"),
            selection: "sel".into(),
            document: "doc".into(),
            line: 3,
            column: 7,
            input: Some("typed".into()),
        }
    }

    #[test]
    fn builds_invocations() {
        let s = spec(
            r#"
            id = "a"
            title = "A"
            program = "bin/tool"
            args = ["${relativeFile}", "${fileBasenameNoExtension}${fileExtname}", "${lineNumber}:${column}", "${input}"]
            stdin = "selection"
            cwd = "${fileDirname}"
            "#,
        );
        let inv = Invocation::new(&s, &ctx()).unwrap();
        assert_eq!(inv.program, Path::new("/ext/t").join("bin/tool"));
        assert_eq!(
            inv.args,
            [
                Path::new("src").join("main.rs").to_string_lossy().as_ref(),
                "main.rs",
                "3:7",
                "typed"
            ]
        );
        assert_eq!(inv.cwd, Path::new("/work/src"));
        assert_eq!(inv.stdin.as_deref(), Some("sel"));
        assert!(
            inv.env
                .iter()
                .any(|(k, v)| k == "FORGE_INPUT" && v == "typed")
        );
        assert_eq!(inv.timeout, Some(Duration::from_secs(60)));

        let untitled = Context {
            file: None,
            ..ctx()
        };
        assert_eq!(
            Invocation::new(&s, &untitled).unwrap_err(),
            "save the file first"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_with_stdin_and_reports_failures() {
        // No folder or file, so it runs in the extension folder.
        let c = Context {
            workspace: None,
            file: None,
            extension_dir: std::env::temp_dir(),
            ..ctx()
        };
        let s = spec(
            "id = \"a\"\ntitle = \"A\"\nprogram = \"tr\"\nargs = [\"a-z\", \"A-Z\"]\nstdin = \"document\"",
        );
        let out = run(&Invocation::new(&s, &c).unwrap()).unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, "DOC");

        let s = spec(
            "id = \"a\"\ntitle = \"A\"\nprogram = \"sh\"\nargs = [\"-c\", \"echo oops >&2; exit 3\"]",
        );
        let out = run(&Invocation::new(&s, &c).unwrap()).unwrap();
        assert!(!out.success);
        assert_eq!(out.code, Some(3));
        assert_eq!(out.error_summary(None), "oops");

        let s = spec("id = \"a\"\ntitle = \"A\"\nprogram = \"sleep\"\nargs = [\"5\"]\ntimeout = 1");
        let out = run(&Invocation::new(&s, &c).unwrap()).unwrap();
        assert!(out.timed_out && !out.success);
        assert!(out.duration < Duration::from_secs(4));

        let s = spec("id = \"a\"\ntitle = \"A\"\nprogram = \"definitely-not-a-program-xyz\"");
        assert!(run(&Invocation::new(&s, &c).unwrap()).is_err());
    }
}

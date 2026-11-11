//! Runs install commands in the background and captures their output.

use std::io::{BufRead as _, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use super::catalog::InstallMethod;
use super::detect::hide_console_window;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    Succeeded,
    Failed(String),
}

#[derive(Debug)]
struct Shared {
    state: JobState,
    log: String,
}

/// A running (or finished) install command.
pub struct InstallJob {
    shared: Arc<Mutex<Shared>>,
}

impl InstallJob {
    /// Starts `method` using the already-resolved `program` path.
    pub fn spawn(method: &InstallMethod, program: &Path, ctx: egui::Context) -> Self {
        let command_line = method.command_line();
        let shared = Arc::new(Mutex::new(Shared {
            state: JobState::Running,
            log: format!("$ {command_line}\n"),
        }));

        let mut cmd = Command::new(program);
        cmd.args(method.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut cmd);

        let job_shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let result = run(cmd, &job_shared, &ctx);
            let mut s = job_shared.lock().unwrap();
            s.state = match result {
                Ok(()) => JobState::Succeeded,
                Err(e) => {
                    s.log.push_str(&format!("\n{e}\n"));
                    JobState::Failed(e)
                }
            };
            drop(s);
            ctx.request_repaint();
        });

        Self { shared }
    }

    pub fn state(&self) -> JobState {
        self.shared.lock().unwrap().state.clone()
    }

    pub fn log(&self) -> String {
        self.shared.lock().unwrap().log.clone()
    }
}

fn run(mut cmd: Command, shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) -> Result<(), String> {
    let mut child = cmd.spawn().map_err(|e| format!("Failed to start: {e}"))?;
    let readers = [
        child
            .stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|reader| {
        let shared = Arc::clone(shared);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let mut s = shared.lock().unwrap();
                s.log.push_str(&line);
                s.log.push('\n');
                drop(s);
                ctx.request_repaint();
            }
        })
    })
    .collect::<Vec<_>>();

    let status = child.wait().map_err(|e| format!("Failed to wait: {e}"))?;
    for r in readers {
        let _ = r.join();
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("Install command exited with {status}"))
    }
}

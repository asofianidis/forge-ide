//! JSON-RPC over stdio with `Content-Length` framing.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::Context as _;
use serde_json::Value;

/// Frames a JSON-RPC message.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Reads one framed message. Returns `Ok(None)` at end of stream.
pub fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut content_length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue; // tolerate stray blank lines between messages
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; content_length.unwrap_or(0)];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// What the reader thread reports.
pub enum Incoming {
    Message(Value),
    /// The server's stdout closed (it exited or crashed). Carries recent stderr.
    Closed(String),
}

/// A running language server process.
pub struct Connection {
    child: Child,
    stdin: ChildStdin,
    pub incoming: Receiver<Incoming>,
}

impl Connection {
    pub fn spawn(
        command: &str,
        args: &[String],
        cwd: Option<&std::path::Path>,
        ctx: &egui::Context,
    ) -> anyhow::Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to start language server `{command}`"))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        // Keep the tail of stderr to explain crashes; draining it also stops the
        // server from blocking on a full pipe.
        let (err_tx, err_rx) = channel::<String>();
        std::thread::Builder::new()
            .name(format!("lsp-stderr-{command}"))
            .spawn(move || {
                let mut tail = std::collections::VecDeque::new();
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    tail.push_back(line);
                    if tail.len() > 20 {
                        tail.pop_front();
                    }
                }
                let _ = err_tx.send(Vec::from(tail).join("\n"));
            })?;

        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name(format!("lsp-reader-{command}"))
            .spawn(move || reader_loop(stdout, &tx, &ctx, &err_rx))?;

        Ok(Self {
            child,
            stdin,
            incoming: rx,
        })
    }

    pub fn send(&mut self, message: &Value) -> std::io::Result<()> {
        self.stdin.write_all(&encode(message))?;
        self.stdin.flush()
    }
}

fn reader_loop(
    stdout: impl Read,
    tx: &Sender<Incoming>,
    ctx: &egui::Context,
    stderr: &Receiver<String>,
) {
    let mut reader = BufReader::new(stdout);
    while let Ok(Some(message)) = read_message(&mut reader) {
        if tx.send(Incoming::Message(message)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
    let stderr = stderr
        .recv_timeout(std::time::Duration::from_millis(500))
        .unwrap_or_default();
    let _ = tx.send(Incoming::Closed(stderr));
    ctx.request_repaint();
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Polite shutdown is best-effort; don't block the UI waiting for it.
        let _ = self.send(&serde_json::json!({"jsonrpc": "2.0", "method": "exit"}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_framed_messages() {
        let a = json!({"jsonrpc": "2.0", "id": 1, "method": "x", "params": {"s": "héllo"}});
        let b = json!({"jsonrpc": "2.0", "method": "y"});
        let mut bytes = encode(&a);
        bytes.extend(encode(&b));
        let mut reader = std::io::Cursor::new(bytes);
        assert_eq!(read_message(&mut reader).unwrap(), Some(a));
        assert_eq!(read_message(&mut reader).unwrap(), Some(b));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn content_length_counts_bytes_not_chars() {
        let bytes = encode(&json!("é"));
        assert!(bytes.starts_with(b"Content-Length: 4\r\n\r\n"));
    }

    #[test]
    fn tolerates_extra_headers() {
        let body = br#"{"id":1}"#;
        let mut input = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        input.extend_from_slice(body);
        let msg = read_message(&mut std::io::Cursor::new(input)).unwrap();
        assert_eq!(msg, Some(json!({"id": 1})));
    }
}

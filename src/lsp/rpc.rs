//! JSON-RPC 2.0 as the Language Server Protocol frames it over stdio: every
//! message is a JSON body preceded by a `Content-Length` header.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

/// Writes one framed message.
pub fn write_message(w: &mut impl Write, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

/// Reads one framed message, or `None` at the end of the stream.
///
/// Header lines other than `Content-Length` (and stray non-header output some
/// servers print before their first message) are ignored.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim();
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse::<usize>().map_err(invalid)?);
        }
    }
    let mut body = vec![0; length.unwrap_or_default()];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(invalid)
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// An error returned in place of a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
}

/// Standard error codes.
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const REQUEST_CANCELLED: i64 = -32800;
pub const CONTENT_MODIFIED: i64 = -32801;

/// A message from the server, sorted by kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The answer to one of our requests.
    Response {
        id: i64,
        result: Result<Value, ResponseError>,
    },
    /// The server asks us something and waits for a reply.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

impl Incoming {
    /// Classifies a raw message. Returns `None` for malformed ones, and for
    /// responses to ids we never send (we only use integers).
    pub fn parse(mut msg: Value) -> Option<Self> {
        let params = msg
            .get_mut("params")
            .map(Value::take)
            .unwrap_or(Value::Null);
        match (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
            (Some(id), Some(method)) => Some(Incoming::Request {
                id: id.clone(),
                method: method.to_owned(),
                params,
            }),
            (None, Some(method)) => Some(Incoming::Notification {
                method: method.to_owned(),
                params,
            }),
            (Some(id), None) => {
                let id = id.as_i64()?;
                let result = match msg.get("error") {
                    Some(err) => Err(ResponseError {
                        code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: err
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_owned(),
                    }),
                    None => Ok(msg
                        .get_mut("result")
                        .map(Value::take)
                        .unwrap_or(Value::Null)),
                };
                Some(Incoming::Response { id, result })
            }
            (None, None) => None,
        }
    }
}

pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

pub fn response(id: Value, result: Result<Value, ResponseError>) -> Value {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": e.code, "message": e.message },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_messages() {
        let mut buf = Vec::new();
        let a = request(1, "initialize", json!({ "x": "é" }));
        let b = notification("initialized", json!({}));
        write_message(&mut buf, &a).unwrap();
        write_message(&mut buf, &b).unwrap();
        let mut r = io::Cursor::new(buf);
        assert_eq!(read_message(&mut r).unwrap(), Some(a));
        assert_eq!(read_message(&mut r).unwrap(), Some(b));
        assert_eq!(read_message(&mut r).unwrap(), None);
    }

    #[test]
    fn tolerates_extra_headers_and_noise() {
        let body = r#"{"jsonrpc":"2.0","method":"x"}"#;
        let raw = format!(
            "some log line\r\ncontent-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{body}",
            body.len()
        );
        let mut r = io::Cursor::new(raw.into_bytes());
        let msg = read_message(&mut r).unwrap().unwrap();
        assert_eq!(msg["method"], "x");
    }

    #[test]
    fn classifies_messages() {
        let resp = Incoming::parse(json!({"jsonrpc":"2.0","id":3,"result":{"a":1}}));
        assert_eq!(
            resp,
            Some(Incoming::Response {
                id: 3,
                result: Ok(json!({"a":1}))
            })
        );
        let err = Incoming::parse(
            json!({"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"nope"}}),
        );
        assert_eq!(
            err,
            Some(Incoming::Response {
                id: 4,
                result: Err(ResponseError {
                    code: METHOD_NOT_FOUND,
                    message: "nope".into()
                })
            })
        );
        let req = Incoming::parse(json!({"jsonrpc":"2.0","id":"a","method":"m","params":[1]}));
        assert!(matches!(req, Some(Incoming::Request { id, .. }) if id == json!("a")));
        let note = Incoming::parse(json!({"jsonrpc":"2.0","method":"n"}));
        assert!(matches!(
            note,
            Some(Incoming::Notification {
                params: Value::Null,
                ..
            })
        ));
    }
}

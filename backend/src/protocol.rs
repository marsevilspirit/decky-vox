use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Write};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub v: u32,
    pub kind: String,
    pub id: u64,
    pub method: String,
    #[serde(default = "empty_object")]
    pub params: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub v: u32,
    pub kind: &'static str,
    pub id: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn success(id: u64, result: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind: "response",
            id: Value::from(id),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Option<u64>, error: ErrorBody) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind: "response",
            id: id.map(Value::from).unwrap_or(Value::Null),
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub v: u32,
    pub kind: &'static str,
    pub instance_id: String,
    pub seq: u64,
    pub name: String,
    pub payload: Value,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("message exceeds {MAX_LINE_BYTES} bytes")]
    TooLarge,
    #[error("malformed JSON: {0}")]
    Malformed(String),
    #[error("kind must be request")]
    WrongKind,
    #[error("unsupported protocol version {0}")]
    VersionMismatch(u32),
}

impl ParseError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "MESSAGE_TOO_LARGE",
            Self::Malformed(_) => "MALFORMED_REQUEST",
            Self::WrongKind => "INVALID_REQUEST",
            Self::VersionMismatch(_) => "PROTOCOL_MISMATCH",
        }
    }
}

pub fn parse_request(line: &[u8]) -> Result<Request, ParseError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(ParseError::TooLarge);
    }
    let request: Request =
        serde_json::from_slice(line).map_err(|error| ParseError::Malformed(error.to_string()))?;
    if request.kind != "request" {
        return Err(ParseError::WrongKind);
    }
    if request.v != PROTOCOL_VERSION {
        return Err(ParseError::VersionMismatch(request.v));
    }
    Ok(request)
}

pub fn write_message<W: Write, T: Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_request() {
        let request =
            parse_request(br#"{"v":1,"kind":"request","id":7,"method":"hello","params":{}}"#)
                .unwrap();
        assert_eq!(request.id, 7);
        assert_eq!(request.method, "hello");
    }

    #[test]
    fn rejects_malformed_and_version_mismatch() {
        assert!(matches!(parse_request(b"{"), Err(ParseError::Malformed(_))));
        assert_eq!(
            parse_request(br#"{"v":2,"kind":"request","id":1,"method":"hello"}"#).unwrap_err(),
            ParseError::VersionMismatch(2)
        );
    }

    #[test]
    fn rejects_oversized_message_before_json_parsing() {
        let line = vec![b'x'; MAX_LINE_BYTES + 1];
        assert_eq!(parse_request(&line).unwrap_err(), ParseError::TooLarge);
    }

    #[test]
    fn writes_exactly_one_json_line() {
        let mut bytes = Vec::new();
        write_message(
            &mut bytes,
            &Response::success(3, serde_json::json!({"ok": true})),
        )
        .unwrap();
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["id"], 3);
    }
}

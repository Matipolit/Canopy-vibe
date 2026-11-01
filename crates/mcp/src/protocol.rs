//! JSON-RPC 2.0 and the part of MCP canopy speaks: `initialize`, `ping`,
//! `tools/list`, `tools/call`, and the cancellation and progress
//! notifications. Messages are single lines of JSON, as MCP's stdio
//! transport sends them; every function here that writes one returns it
//! without the newline.

use serde_json::{Value, json};

/// A request's id: a number or a string, echoed back in the reply. `null`
/// for errors about a message whose id couldn't be read.
pub type Id = Value;

/// The protocol versions canopy understands, newest first. A client asking
/// for another gets the newest, and decides for itself whether it can
/// carry on.
pub const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The longest message accepted, in bytes. Longer lines are refused before
/// they are parsed, so a runaway client can't make canopy hold gigabytes.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// JSON-RPC's error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// A message from the client.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Wants a reply carrying `id`.
    Request {
        id: Id,
        method: String,
        params: Value,
    },
    /// Wants none (`notifications/initialized`, `notifications/cancelled`).
    Notification { method: String, params: Value },
}

/// Reads one line from the client. The error is a ready reply: a parse
/// error for text that isn't JSON, an invalid request for JSON that isn't a
/// request or a notification. Missing `params` read as an empty object.
pub fn parse(line: &str) -> Result<Incoming, String> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| error(&Value::Null, PARSE_ERROR, &format!("not JSON: {e}")))?;
    let Value::Object(mut message) = value else {
        return Err(error(
            &Value::Null,
            INVALID_REQUEST,
            "a message is one JSON object (batches aren't supported)",
        ));
    };
    let id = message.remove("id");
    let Some(Value::String(method)) = message.remove("method") else {
        return Err(error(
            id.as_ref().unwrap_or(&Value::Null),
            INVALID_REQUEST,
            "a request needs a `method` string",
        ));
    };
    let params = match message.remove("params") {
        None | Some(Value::Null) => json!({}),
        Some(params) => params,
    };
    Ok(match id {
        Some(id) => Incoming::Request { id, method, params },
        None => Incoming::Notification { method, params },
    })
}

/// A successful reply.
pub fn result(id: &Id, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

/// A failed reply: a protocol error, not a tool's (those are results with
/// `isError`, see [`tool_result`]).
pub fn error(id: &Id, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

/// A message that wants no reply, such as `notifications/progress`.
pub fn notification(method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()
}

/// The result of `initialize`: the version both sides will speak (the
/// client's if canopy knows it), that canopy offers tools, and how to use
/// them (`instructions`, which clients show the model).
pub fn initialize(requested: Option<&str>, instructions: &str) -> Value {
    let version = requested
        .filter(|v| VERSIONS.contains(v))
        .unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "canopy", "version": env!("CARGO_PKG_VERSION") },
        "instructions": instructions,
    })
}

/// A piece of a tool's result.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    /// A PNG, sent as base64.
    Image {
        png: Vec<u8>,
    },
}

/// The result of `tools/call`. A tool that failed still answers with a
/// result, flagged `isError`, so the model reads the message and can try
/// again; protocol errors are for calls that never reached a tool.
pub fn tool_result(content: &[Content], is_error: bool) -> Value {
    let content: Vec<Value> = content
        .iter()
        .map(|c| match c {
            Content::Text(text) => json!({ "type": "text", "text": text }),
            Content::Image { png } => json!({
                "type": "image",
                "mimeType": "image/png",
                "data": base64(png),
            }),
        })
        .collect();
    json!({ "content": content, "isError": is_error })
}

/// Standard base64 with padding (RFC 4648), for images. Written out rather
/// than taken from a crate: it is a dozen lines.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_of(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn requests_and_notifications_parse() {
        let request =
            parse(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{"cursor":null}}"#)
                .unwrap();
        assert_eq!(
            request,
            Incoming::Request {
                id: json!(7),
                method: "tools/list".into(),
                params: json!({ "cursor": null }),
            }
        );
        let notification =
            parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert_eq!(
            notification,
            Incoming::Notification {
                method: "notifications/initialized".into(),
                params: json!({}),
            }
        );
        // String ids are echoed as they came.
        let Incoming::Request { id, .. } =
            parse(r#"{"jsonrpc":"2.0","id":"a-1","method":"ping"}"#).unwrap()
        else {
            panic!("a request");
        };
        assert_eq!(
            json_of(&result(&id, json!({}))),
            json!({ "jsonrpc": "2.0", "id": "a-1", "result": {} })
        );
    }

    #[test]
    fn malformed_json_gets_a_parse_error() {
        let line = parse("{\"jsonrpc\": \"2.0\", \"id\": 1, ").unwrap_err();
        assert!(!line.contains('\n'));
        let reply = json_of(&line);
        assert_eq!(reply["error"]["code"], json!(-32700));
        assert_eq!(reply["id"], Value::Null);
    }

    #[test]
    fn a_request_without_method_is_invalid() {
        let reply = json_of(&parse(r#"{"jsonrpc":"2.0","id":3}"#).unwrap_err());
        assert_eq!(reply["error"]["code"], json!(-32600));
        assert_eq!(reply["id"], json!(3), "the id is kept when there is one");
        let reply = json_of(&parse("[1, 2]").unwrap_err());
        assert_eq!(reply["error"]["code"], json!(-32600));
        assert_eq!(reply["id"], Value::Null);
    }

    #[test]
    fn initialize_echoes_a_known_version() {
        let known = initialize(Some("2025-06-18"), "Be kind.");
        assert_eq!(known["protocolVersion"], json!("2025-06-18"));
        assert_eq!(known["capabilities"], json!({ "tools": {} }));
        assert_eq!(known["serverInfo"]["name"], json!("canopy"));
        assert_eq!(known["instructions"], json!("Be kind."));
        let unknown = initialize(Some("1999-01-01"), "");
        assert_eq!(unknown["protocolVersion"], json!("2025-11-25"));
        assert_eq!(initialize(None, "")["protocolVersion"], json!("2025-11-25"));
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
        assert_eq!(base64(&[0xFF, 0xFE, 0xFD]), "//79");
    }

    #[test]
    fn images_are_base64_png_content() {
        let result = tool_result(
            &[
                Content::Text("{\"peak\":1000}".into()),
                Content::Image {
                    png: b"foobar".to_vec(),
                },
            ],
            false,
        );
        assert_eq!(result["isError"], json!(false));
        assert_eq!(
            result["content"][0],
            json!({ "type": "text", "text": "{\"peak\":1000}" })
        );
        assert_eq!(
            result["content"][1],
            json!({ "type": "image", "mimeType": "image/png", "data": "Zm9vYmFy" })
        );
        let failed = tool_result(&[Content::Text("no".into())], true);
        assert_eq!(failed["isError"], json!(true));
    }

    #[test]
    fn notifications_and_errors_are_single_lines() {
        let progress = notification("notifications/progress", json!({ "progress": 0.5 }));
        assert!(!progress.contains('\n'));
        let progress = json_of(&progress);
        assert_eq!(progress["method"], json!("notifications/progress"));
        assert!(progress.get("id").is_none());
        let failed = json_of(&error(&json!(9), METHOD_NOT_FOUND, "no such method"));
        assert_eq!(failed["error"]["code"], json!(-32601));
        assert_eq!(failed["error"]["message"], json!("no such method"));
    }
}

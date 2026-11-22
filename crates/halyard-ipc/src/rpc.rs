//! JSON-RPC 2.0 framing, error codes, and strict request parsing.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// JSON-RPC error codes used by the session API. See `docs/session-api.md`.
pub mod code {
    /// The line is not valid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Valid JSON but not a valid request: wrong envelope, unknown envelope field, missing
    /// or null id, batch, or a repeated `halyard.hello`.
    pub const INVALID_REQUEST: i64 = -32600;
    /// No such method.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Params do not match the method's schema (including unknown fields).
    pub const INVALID_PARAMS: i64 = -32602;
    /// A server bug.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// A method was called before `halyard.hello`.
    pub const NOT_INITIALIZED: i64 = -32000;
    /// No protocol version in common.
    pub const UNSUPPORTED_PROTOCOL: i64 = -32001;
    /// No identity yet, a bad token, or a revoked grant.
    pub const UNAUTHENTICATED: i64 = -32002;
    /// Authenticated, but this identity may not do this (missing scope, other session).
    pub const FORBIDDEN: i64 = -32003;
    /// The session does not exist (or no longer exists).
    pub const SESSION_NOT_FOUND: i64 = -32004;
    /// The user declined the request in the approval dialog.
    pub const DENIED: i64 = -32005;
    /// Too many requests on this connection; slow down.
    pub const RATE_LIMITED: i64 = -32006;
    /// The message exceeded the size limit. The connection is closed after this error.
    pub const MESSAGE_TOO_LARGE: i64 = -32007;
    /// The connecting process is not inside the session it claims.
    pub const NOT_IN_SESSION: i64 = -32008;
    /// The app could not perform the action (pane busy, focus failed, ...).
    pub const UNAVAILABLE: i64 = -32009;
}

/// An error response body.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    /// One of the [`code`] constants.
    pub code: i64,
    /// A human-readable message.
    pub message: String,
    /// Optional structured detail.
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with no data.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }

    /// Attaches structured detail.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The JSON `error` object.
    pub fn to_json(&self) -> Value {
        match &self.data {
            Some(data) => json!({ "code": self.code, "message": self.message, "data": data }),
            None => json!({ "code": self.code, "message": self.message }),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// A parsed, well-formed request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The request id (a number or a string).
    pub id: Value,
    /// The method name.
    pub method: String,
    /// The params object (`{}` when absent).
    pub params: Value,
}

/// Parses one line into a request, or the error response to send (with the id to use).
// The error is large only because `serde_json::Value` grows when another workspace crate
// enables serde_json features (feature unification). It is built at most once per
// request line, on the error path, so boxing it would buy nothing.
#[allow(clippy::result_large_err)]
pub fn parse_request(line: &[u8]) -> Result<Request, (Value, RpcError)> {
    let value: Value = serde_json::from_slice(line).map_err(|e| {
        (Value::Null, RpcError::new(code::PARSE_ERROR, format!("parse error: {e}")))
    })?;
    if value.is_array() {
        return Err((
            Value::Null,
            RpcError::new(code::INVALID_REQUEST, "batch requests are not supported"),
        ));
    }
    // Recover the id, if usable, so the error can be correlated.
    let raw_id = value.get("id").filter(|id| id.is_number() || id.is_string()).cloned();
    let err_id = raw_id.clone().unwrap_or(Value::Null);
    let env: Envelope = serde_json::from_value(value)
        .map_err(|e| (err_id.clone(), RpcError::new(code::INVALID_REQUEST, e.to_string())))?;
    if env.jsonrpc != "2.0" {
        return Err((err_id, RpcError::new(code::INVALID_REQUEST, "jsonrpc must be \"2.0\"")));
    }
    let Some(id) = raw_id else {
        let msg = if env.id.is_none() {
            "notifications are not accepted; every request needs an id"
        } else {
            "id must be a number or a string"
        };
        return Err((Value::Null, RpcError::new(code::INVALID_REQUEST, msg)));
    };
    let params = match env.params {
        None => json!({}),
        Some(p @ Value::Object(_)) => p,
        Some(_) => {
            return Err((id, RpcError::new(code::INVALID_PARAMS, "params must be an object")));
        }
    };
    Ok(Request { id, method: env.method, params })
}

/// Deserializes method params strictly (the target type uses `deny_unknown_fields`).
pub fn params<T: DeserializeOwned>(value: &Value) -> Result<T, RpcError> {
    serde_json::from_value(value.clone())
        .map_err(|e| RpcError::new(code::INVALID_PARAMS, format!("invalid params: {e}")))
}

/// A success response line (without the trailing newline).
pub fn response_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// An error response line (without the trailing newline).
pub fn response_err(id: &Value, error: &RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": error.to_json() })
}

/// A server-to-client notification.
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_code(line: &str) -> i64 {
        parse_request(line.as_bytes()).expect_err(line).1.code
    }

    #[test]
    fn accepts_a_well_formed_request() {
        let r = parse_request(br#"{"jsonrpc":"2.0","id":7,"method":"m"}"#).expect("valid");
        assert_eq!(r.id, json!(7));
        assert_eq!(r.params, json!({}));
    }

    #[test]
    fn rejects_malformed_envelopes() {
        assert_eq!(err_code("{nope"), code::PARSE_ERROR);
        assert_eq!(err_code("[]"), code::INVALID_REQUEST);
        assert_eq!(err_code(r#"{"jsonrpc":"1.0","id":1,"method":"m"}"#), code::INVALID_REQUEST);
        assert_eq!(err_code(r#"{"jsonrpc":"2.0","method":"m"}"#), code::INVALID_REQUEST);
        assert_eq!(err_code(r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#), code::INVALID_REQUEST);
        assert_eq!(
            err_code(r#"{"jsonrpc":"2.0","id":1,"method":"m","extra":true}"#),
            code::INVALID_REQUEST
        );
        assert_eq!(
            err_code(r#"{"jsonrpc":"2.0","id":1,"method":"m","params":[1]}"#),
            code::INVALID_PARAMS
        );
    }

    #[test]
    fn error_keeps_a_usable_id() {
        let (id, _) = parse_request(br#"{"jsonrpc":"2.0","id":"a","method":"m","x":1}"#)
            .expect_err("unknown envelope field");
        assert_eq!(id, json!("a"));
    }
}

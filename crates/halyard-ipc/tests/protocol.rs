//! Framing, versioning, strict schemas and limits, over a real socket.

mod common;

use std::time::Duration;

use common::{code_of, start, start_with};
use halyard_ipc::{Limits, MAX_MESSAGE_BYTES, code};
use serde_json::json;

#[test]
fn hello_negotiates_protocol_1() {
    let h = start();
    let mut c = h.connect();
    let result = c.hello("test").expect("hello");
    assert_eq!(result["protocol"], json!(1));
    assert_eq!(result["server"], json!("halyard"));
    assert_eq!(result["limits"]["max_message_bytes"], json!(MAX_MESSAGE_BYTES));
    // A second hello on the same connection is refused.
    assert_eq!(code_of(c.hello("again")), code::INVALID_REQUEST);
}

#[test]
fn hello_without_a_common_version_fails_with_the_supported_list() {
    let h = start();
    let mut c = h.connect();
    let err = c
        .call("halyard.hello", json!({ "protocols": [2, 3], "client_name": "future" }))
        .expect_err("no common version");
    let halyard_ipc::ClientError::Rpc(rpc) = err else { panic!("{err}") };
    assert_eq!(rpc.code, code::UNSUPPORTED_PROTOCOL);
    assert_eq!(rpc.data, Some(json!({ "supported": [1] })));
    // A newer client that also offers 1 gets 1.
    let r =
        c.call("halyard.hello", json!({ "protocols": [1, 2], "client_name": "x" })).expect("ok");
    assert_eq!(r["protocol"], json!(1));
}

#[test]
fn methods_before_hello_are_refused() {
    let h = start();
    let mut c = h.connect();
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::NOT_INITIALIZED);
    assert_eq!(code_of(c.call("no.such", json!({}))), code::METHOD_NOT_FOUND);
}

#[test]
fn malformed_json_gets_a_parse_error_and_the_connection_survives() {
    let h = start();
    let mut c = h.hello();
    c.send_raw(b"{this is not json\n").expect("send");
    let msg = c.read_message().expect("error response");
    assert_eq!(msg["error"]["code"], json!(code::PARSE_ERROR));
    assert_eq!(msg["id"], json!(null));
    c.send_raw(b"[1,2]\n").expect("send");
    assert_eq!(c.read_message().expect("resp")["error"]["code"], json!(code::INVALID_REQUEST));
    c.send_raw(b"{\"jsonrpc\":\"2.0\",\"method\":\"halyard.hello\"}\n").expect("send");
    assert_eq!(c.read_message().expect("resp")["error"]["code"], json!(code::INVALID_REQUEST));
    // Still usable.
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::UNAUTHENTICATED);
}

#[test]
fn unknown_method_is_method_not_found() {
    let h = start();
    let mut c = h.hello();
    assert_eq!(code_of(c.call("session.delete_everything", json!({}))), code::METHOD_NOT_FOUND);
}

#[test]
fn unknown_fields_are_rejected() {
    let h = start();
    let mut c = h.connect();
    let r = c.call("halyard.hello", json!({ "protocols": [1], "client_name": "x", "admin": true }));
    assert_eq!(code_of(r), code::INVALID_PARAMS);
    c.hello("x").expect("hello");
    c.send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"sessions.list\",\"sudo\":1}\n")
        .expect("send");
    let msg = c.read_message().expect("resp");
    assert_eq!(msg["error"]["code"], json!(code::INVALID_REQUEST));
    assert_eq!(msg["id"], json!(9));
    let r = c.call("halyard.pair", json!({ "client_name": "x", "scopes": ["root:all"] }));
    assert_eq!(code_of(r), code::INVALID_PARAMS, "unknown scope names are rejected");
}

#[test]
fn message_at_the_limit_is_accepted() {
    let h = start();
    let mut c = h.connect();
    let mut line =
        br#"{"jsonrpc":"2.0","id":1,"method":"halyard.hello","params":{"protocols":[1],"client_name":"x"}}"#
            .to_vec();
    line.resize(MAX_MESSAGE_BYTES, b' ');
    line.push(b'\n');
    c.send_raw(&line).expect("send");
    let msg = c.read_message().expect("resp");
    assert_eq!(msg["result"]["protocol"], json!(1), "{msg}");
}

#[test]
fn oversized_message_is_rejected_and_the_connection_closed() {
    let h = start();
    let mut c = h.hello();
    let mut line = vec![b' '; MAX_MESSAGE_BYTES + 1];
    line.push(b'\n');
    // The server may close while we are still writing; that is fine.
    let _ = c.send_raw(&line);
    let msg = c.read_message().expect("too-large error");
    assert_eq!(msg["error"]["code"], json!(code::MESSAGE_TOO_LARGE));
    assert!(c.read_message().is_err(), "connection is closed after an oversized message");
}

#[test]
fn rate_limit_rejects_bursts_then_disconnects_abusers() {
    let limits =
        Limits { rate_burst: 3, rate_per_sec: 1, max_rate_violations: 2, ..Limits::default() };
    let h = start_with(limits);
    let mut c = h.hello(); // uses 1 token
    let mut codes = Vec::new();
    for _ in 0..4 {
        codes.push(match c.call("sessions.list", json!({})) {
            Ok(_) => 0,
            Err(e) => e.rpc_code().expect("rpc error"),
        });
    }
    assert_eq!(
        codes,
        vec![code::UNAUTHENTICATED, code::UNAUTHENTICATED, code::RATE_LIMITED, code::RATE_LIMITED]
    );
    // Third violation: answered, then the connection is closed.
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::RATE_LIMITED);
    assert!(c.call("sessions.list", json!({})).is_err());
    assert!(c.read_message().is_err());

    // Tokens refill: a new connection after a pause is served normally.
    std::thread::sleep(Duration::from_millis(1100));
    let mut c2 = h.hello();
    assert_eq!(code_of(c2.call("sessions.list", json!({}))), code::UNAUTHENTICATED);
}

#[test]
fn handshake_timeout_closes_silent_connections() {
    let limits = Limits { handshake_timeout: Duration::from_millis(200), ..Limits::default() };
    let h = start_with(limits);
    let mut c = h.connect();
    std::thread::sleep(Duration::from_millis(500));
    // The server has hung up; a write may or may not error, the read must.
    let _ = c.send_raw(b"\n");
    assert!(c.read_message().is_err());
}

#[test]
fn connection_limit_closes_extra_connections() {
    let limits = Limits { max_connections: 2, ..Limits::default() };
    let h = start_with(limits);
    let _a = h.hello();
    let _b = h.hello();
    let mut c = h.connect();
    let _ = c.send_raw(b"{}\n");
    assert!(c.read_message().is_err(), "third connection is closed");
}

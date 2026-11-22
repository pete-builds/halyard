//! A small blocking client, used by `halyard-msg` and the tests.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::PROTOCOL_VERSION;
use crate::rpc::RpcError;

/// A client-side failure.
#[derive(Debug)]
pub enum ClientError {
    /// Socket I/O failed or timed out.
    Io(io::Error),
    /// The server answered with an error.
    Rpc(RpcError),
    /// The server sent something that is not a valid response.
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "socket error: {e}"),
            Self::Rpc(e) => write!(f, "server error: {e}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl ClientError {
    /// The JSON-RPC error code, when the server answered with an error.
    pub fn rpc_code(&self) -> Option<i64> {
        match self {
            Self::Rpc(e) => Some(e.code),
            _ => None,
        }
    }
}

/// A connection to a Halyard session API socket.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    notifications: VecDeque<Value>,
}

impl Client {
    /// Connects. `timeout` bounds every later read and write.
    pub fn connect(path: &Path, timeout: Option<Duration>) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            notifications: VecDeque::new(),
        })
    }

    /// Sends `halyard.hello` offering protocol version 1.
    pub fn hello(&mut self, client_name: &str) -> Result<Value, ClientError> {
        self.call(
            "halyard.hello",
            json!({ "protocols": [PROTOCOL_VERSION], "client_name": client_name }),
        )
    }

    /// Sends a raw line (no newline added). For tests of malformed input.
    pub fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)
    }

    /// Reads the next line from the server as JSON.
    pub fn read_message(&mut self) -> Result<Value, ClientError> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(ClientError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed the connection",
            )));
        }
        serde_json::from_str(&line).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// Calls `method` and waits for its response. Notifications that arrive meanwhile are
    /// queued for [`Client::next_notification`].
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }))
        .map_err(|e| ClientError::Protocol(e.to_string()))?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        loop {
            let msg = self.read_message()?;
            if msg.get("id").is_none() {
                self.notifications.push_back(msg);
                continue;
            }
            if msg["id"] != json!(id) {
                return Err(ClientError::Protocol(format!("unexpected response {msg}")));
            }
            return parse_response(msg);
        }
    }

    /// The next server notification (`{"method": ..., "params": ...}`), waiting for one if
    /// none is queued.
    pub fn next_notification(&mut self) -> Result<Value, ClientError> {
        if let Some(n) = self.notifications.pop_front() {
            return Ok(n);
        }
        loop {
            let msg = self.read_message()?;
            if msg.get("id").is_none() {
                return Ok(msg);
            }
        }
    }
}

/// Splits a response into its result or its error.
pub fn parse_response(msg: Value) -> Result<Value, ClientError> {
    if let Some(err) = msg.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = err.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
        let mut rpc = RpcError::new(code, message);
        rpc.data = err.get("data").cloned();
        return Err(ClientError::Rpc(rpc));
    }
    msg.get("result").cloned().ok_or_else(|| ClientError::Protocol(format!("no result in {msg}")))
}

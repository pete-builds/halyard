//! The escape-sequence fallback `halyard-msg` writes to `/dev/tty` when no socket is
//! reachable.
//!
//! State reports use OSC 7717 exactly as `docs/protocol.md` defines it:
//! `ESC ] 7717 ; key=value;key=value ESC \`, with the keys `state`, `agent`, `msg` and
//! `session`. That document percent-encodes `%`, `;`, bytes below 0x20 and 0x7f, and its
//! decoders accept any `%HH`. This encoder escapes more (see [`encode_value`]) so the
//! payload is printable ASCII only: no value can end the sequence early (ESC, BEL), smuggle
//! another escape sequence, or carry a C1 control. Output stays valid under that spec.
//!
//! Notifications use `ESC ] 777 ; notify ; title ; body ESC \`, which the shell scanner
//! (and other terminals) already read, with `;` and control characters replaced, since
//! OSC 777 has no escaping.

use crate::text::sanitize_display;
use crate::types::AgentState;

/// The OSC number for Halyard state reports.
pub const OSC_STATE: u32 = 7717;

/// Percent-encodes a value: unreserved printable ASCII passes through; `%`, `;`, `=`,
/// space, every control byte and every non-ASCII byte become `%XX`.
pub fn encode_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_graphic() && !matches!(b, b'%' | b';' | b'=') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes [`encode_value`] output. `None` on a malformed escape or invalid UTF-8.
pub fn decode_value(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The full OSC 7717 state sequence. `session` is the `HALYARD_SESSION_ID` the caller saw;
/// Halyard drops the report if it reaches a pane with a different id.
pub fn state_sequence(
    state: AgentState,
    agent: Option<&str>,
    msg: Option<&str>,
    session: Option<&str>,
) -> String {
    let mut payload = format!("state={}", state.as_str());
    if let Some(agent) = agent {
        payload.push_str(&format!(";agent={}", encode_value(agent)));
    }
    if let Some(msg) = msg {
        payload.push_str(&format!(";msg={}", encode_value(msg)));
    }
    if let Some(session) = session {
        payload.push_str(&format!(";session={}", encode_value(session)));
    }
    format!("\u{1b}]{OSC_STATE};{payload}\u{1b}\\")
}

/// The full OSC 777 notification sequence.
pub fn notify_sequence(title: &str, body: &str) -> String {
    let clean = |s: &str, max| sanitize_display(s, max).replace(';', ",");
    format!("\u{1b}]777;notify;{};{}\u{1b}\\", clean(title, 256), clean(body, 4096))
}

/// Parses an OSC 7717 payload (the part after `7717;`) into key/value pairs.
pub fn parse_payload(payload: &str) -> Option<Vec<(String, String)>> {
    payload
        .split(';')
        .map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((k.to_owned(), decode_value(v)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_sequence_round_trips_hostile_text() {
        let msg = "a;b=c%d\u{1b}]0;evil\u{7}\nné";
        let seq = state_sequence(AgentState::Waiting, Some("claude-code"), Some(msg), Some("s-1"));
        assert!(seq.starts_with("\u{1b}]7717;"));
        assert!(seq.ends_with("\u{1b}\\"));
        let payload = &seq["\u{1b}]7717;".len()..seq.len() - 2];
        assert!(payload.bytes().all(|b| b.is_ascii_graphic()), "payload is printable ASCII");
        let pairs = parse_payload(payload).expect("parse");
        assert_eq!(
            pairs,
            vec![
                ("state".into(), "waiting".into()),
                ("agent".into(), "claude-code".into()),
                ("msg".into(), msg.into()),
                ("session".into(), "s-1".into()),
            ]
        );
    }

    #[test]
    fn notify_sequence_has_no_control_bytes_inside() {
        let seq = notify_sequence("t;i\u{1b}tle", "bo\ndy");
        let inner = &seq[2..seq.len() - 2];
        assert!(!inner.chars().any(|c| c.is_control()), "{inner:?}");
        assert_eq!(inner, "777;notify;t,i tle;bo dy");
    }
}

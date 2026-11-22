//! Helpers that make untrusted text safe to keep and display.

/// Length caps, in characters, for strings extracted from terminal output.
pub mod limits {
    /// Window and icon titles.
    pub const TITLE: usize = 256;
    /// Notification titles and bodies.
    pub const NOTIFICATION: usize = 1024;
    /// A command line reported by OSC 633;E or OSC 133;C `cmdline_url`.
    pub const COMMAND_LINE: usize = 4096;
    /// A working directory path from OSC 7.
    pub const PATH: usize = 4096;
    /// A host name from OSC 7.
    pub const HOST: usize = 255;
    /// An `aid=` application id on an OSC 133 mark.
    pub const AID: usize = 64;
    /// The `agent=` field of the Halyard protocol.
    pub const AGENT_NAME: usize = 64;
    /// The `msg=` field of the Halyard protocol.
    pub const AGENT_MESSAGE: usize = 1024;
    /// The `session=` field of the Halyard protocol.
    pub const SESSION_ID: usize = 128;
}

/// Returns true for characters that must never reach a stored or displayed string:
/// C0 and C1 controls, DEL, and the Unicode bidirectional overrides and isolates that
/// can make text render differently from its logical order.
pub fn is_forbidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Removes forbidden characters (see [`is_forbidden`]) and truncates to at most
/// `max_chars` characters, never splitting a character.
pub fn sanitize(input: &str, max_chars: usize) -> String {
    input.chars().filter(|c| !is_forbidden(*c)).take(max_chars).collect()
}

/// Decodes bytes as UTF-8 (replacing invalid sequences) and sanitizes the result.
pub fn sanitize_bytes(input: &[u8], max_chars: usize) -> String {
    sanitize(&String::from_utf8_lossy(input), max_chars)
}

/// Percent-decodes `%HH` escapes. A `%` not followed by two hex digits is kept as is.
pub fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%'
            && i + 2 < input.len()
            && let (Some(h), Some(l)) = (hex_val(input[i + 1]), hex_val(input[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

/// Value of an ASCII hex digit.
pub(crate) fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_and_bidi() {
        assert_eq!(sanitize("a\x1b[31mb\x07c\u{9b}d\u{202E}e\u{2066}f\x7f", 100), "a[31mbcdef");
        assert_eq!(sanitize("tab\there\nnew", 100), "tabherenew");
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(sanitize("héllo wörld", 4), "héll");
        assert_eq!(sanitize("日本語テキスト", 3), "日本語");
        assert_eq!(sanitize("", 3), "");
    }

    #[test]
    fn lossy_bytes() {
        assert_eq!(sanitize_bytes(b"ok\xff\x01!", 10), "ok\u{FFFD}!");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode(b"a%20b%2Fc"), b"a b/c");
        assert_eq!(percent_decode(b"%C3%A9"), "é".as_bytes());
        assert_eq!(percent_decode(b"100%"), b"100%");
        assert_eq!(percent_decode(b"%2"), b"%2");
        assert_eq!(percent_decode(b"%zz%4"), b"%zz%4");
        assert_eq!(percent_decode(b"%41%"), b"A%");
    }
}

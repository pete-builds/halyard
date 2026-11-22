//! Validation of untrusted text: names, display strings, and text to be typed into a pane.

/// Maximum length, in characters, of a `msg` in a state report.
pub const MAX_MSG_CHARS: usize = 1024;
/// Maximum length, in characters, of a notification title.
pub const MAX_TITLE_CHARS: usize = 256;
/// Maximum length, in characters, of a notification body.
pub const MAX_BODY_CHARS: usize = 4096;
/// Maximum length, in bytes, of text sent with `session.send_text`.
pub const MAX_SEND_TEXT_BYTES: usize = 16 * 1024;

/// True for characters that are invisible or reorder text: zero-width characters, bidi
/// embeddings, overrides and isolates, and the byte order mark. They can make an approval
/// dialog show something other than what will be typed.
pub fn is_deceptive_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// True when typing `text` into a pane could do more than insert those characters.
///
/// That is: any C0 control (which includes newline, carriage return, tab, escape, Ctrl-C,
/// Ctrl-O, which accepts the line in bash, and Ctrl-J), DEL, any C1 control, or a
/// deceptive format character. Such text always needs a fresh per-request approval, even
/// for a client the user marked "always allow".
pub fn needs_fresh_approval(text: &str) -> bool {
    text.chars().any(|c| c.is_control() || is_deceptive_format_char(c))
}

/// Renders `text` so every character is visible: controls become `\n`, `\r`, `\t`, `\e`
/// or `\u{..}`, deceptive format characters become `\u{..}`, and a backslash becomes `\\`.
/// Approval dialogs should show this form.
pub fn escape_for_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{1b}' => out.push_str("\\e"),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() || is_deceptive_format_char(c) => {
                out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Checks a display string (message, title, client name): no control or deceptive format
/// characters, at most `max_chars` characters.
pub fn check_display(field: &str, value: &str, max_chars: usize) -> Result<(), String> {
    if value.chars().count() > max_chars {
        return Err(format!("{field} is longer than {max_chars} characters"));
    }
    if value.chars().any(|c| c.is_control() || is_deceptive_format_char(c)) {
        return Err(format!("{field} contains control or invisible formatting characters"));
    }
    Ok(())
}

/// Checks an identifier (agent name, session id): 1 to `max` bytes of `[A-Za-z0-9._-]`, the
/// session id alphabet of `docs/protocol.md`.
pub fn check_ident(field: &str, value: &str, max: usize) -> Result<(), String> {
    let ok = !value.is_empty()
        && value.len() <= max
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!("{field} must be 1 to {max} characters of [A-Za-z0-9._-]"))
    }
}

/// Replaces control and deceptive format characters with spaces. Used by `halyard-msg`
/// so a hook passing multi-line text still gets its message through.
pub fn sanitize_display(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .map(|c| if c.is_control() || is_deceptive_format_char(c) { ' ' } else { c })
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_needs_no_fresh_approval() {
        assert!(!needs_fresh_approval("git status"));
        assert!(!needs_fresh_approval("héllo wörld 日本"));
    }

    #[test]
    fn controls_and_invisibles_need_fresh_approval() {
        for text in [
            "ls\n",
            "ls\r",
            "a\tb",
            "\u{1b}[A",
            "\u{3}",
            "\u{f}",
            "\u{7f}",
            "\u{9b}",
            "a\u{202e}b",
            "a\u{200b}b",
        ] {
            assert!(needs_fresh_approval(text), "{text:?}");
        }
    }

    #[test]
    fn display_escaping_shows_everything() {
        assert_eq!(escape_for_display("rm -rf ~\n"), "rm -rf ~\\n");
        assert_eq!(escape_for_display("a\u{202e}b\\"), "a\\u{202e}b\\\\");
        assert_eq!(escape_for_display("\u{1b}x\u{3}"), "\\ex\\u{0003}");
    }

    #[test]
    fn idents_and_display_strings() {
        assert!(check_ident("agent", "claude-code", 64).is_ok());
        assert!(check_ident("agent", "", 64).is_err());
        assert!(check_ident("agent", "a b", 64).is_err());
        assert!(check_display("msg", "Needs approval", 10).is_err());
        assert!(check_display("msg", "ok", 10).is_ok());
        assert!(check_display("msg", "a\nb", 10).is_err());
        assert_eq!(sanitize_display("a\nb\u{1b}", 10), "a b ");
    }
}

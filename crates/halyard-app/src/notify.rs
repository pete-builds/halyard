//! Sending a desktop notification. The decision (when, how often, what text) is
//! [`crate::attention_ui::Notifier`]; this only delivers a [`Note`] whose text is
//! already sanitized.
//!
//! * macOS: `NSUserNotificationCenter` through the Objective-C runtime (no
//!   `osascript`). It needs a bundle identifier, so it works from `Halyard.app`;
//!   a bare binary (`cargo run`) has none and reports that instead. GPUI 0.2.2 has
//!   no notification API. macOS shows nothing while Halyard is the frontmost app
//!   (the in-app unread pill covers that case; Halyard only notifies when its
//!   window is not active anyway). The API is deprecated since macOS 11;
//!   moving to `UNUserNotificationCenter` (which needs an authorization request
//!   and a signed bundle) is a follow-up.
//! * Linux: the freedesktop `org.freedesktop.Notifications.Notify` call over the
//!   session D-Bus, from one worker thread with one reused connection. Title and
//!   body are markup-escaped ([`escape_markup`]): notification servers render
//!   markup in them, and the text comes from the pane.
//! * Elsewhere: not supported, reported as such.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::attention_ui::Note;

/// Reports a failed notification on stderr, once per run: the reason (no app
/// bundle, no notification daemon) does not change while the app runs, and the
/// pane's own marks still show the state.
pub fn report_failure(error: &str) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::Relaxed) {
        eprintln!("halyard: desktop notification not sent: {error}");
    }
}

/// Delivers `note`. Errors are returned for the caller to show once; they never
/// stop the terminal.
#[cfg(target_os = "macos")]
// objc's `msg_send!` expands a `cargo-clippy` cfg check (as in debug_script.rs).
#[allow(unexpected_cfgs)]
pub fn send(note: &Note) -> Result<(), String> {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    /// An autoreleased `NSString` from UTF-8 text without interior NULs.
    unsafe fn ns_string(s: &str) -> *mut Object {
        let Some(cls) = Class::get("NSString") else { return std::ptr::null_mut() };
        let bytes = s.replace('\0', "");
        // SAFETY: `stringWithUTF8String:` copies the NUL-terminated buffer, which
        // lives until the call returns.
        let c = std::ffi::CString::new(bytes).unwrap_or_default();
        unsafe { msg_send![cls, stringWithUTF8String: c.as_ptr()] }
    }

    let (Some(bundle_cls), Some(center_cls), Some(note_cls)) = (
        Class::get("NSBundle"),
        Class::get("NSUserNotificationCenter"),
        Class::get("NSUserNotification"),
    ) else {
        return Err("desktop notifications are not available on this macOS".into());
    };
    // SAFETY: plain Objective-C message sends to Foundation classes that exist
    // (checked above), with object arguments that are valid or nil. Runs on the
    // main thread (the UI thread calls this).
    unsafe {
        let bundle: *mut Object = msg_send![bundle_cls, mainBundle];
        let ident: *mut Object = if bundle.is_null() {
            std::ptr::null_mut()
        } else {
            msg_send![bundle, bundleIdentifier]
        };
        if ident.is_null() {
            return Err("desktop notifications need Halyard.app on macOS (this binary has no bundle identifier)".into());
        }
        let center: *mut Object = msg_send![center_cls, defaultUserNotificationCenter];
        if center.is_null() {
            return Err("the macOS notification center is not available".into());
        }
        let n: *mut Object = msg_send![note_cls, new];
        if n.is_null() {
            return Err("could not create a notification".into());
        }
        let title = ns_string(&note.title);
        let body = ns_string(&note.body);
        let _: () = msg_send![n, setTitle: title];
        let _: () = msg_send![n, setInformativeText: body];
        let _: () = msg_send![center, deliverNotification: n];
        let _: () = msg_send![n, release];
    }
    Ok(())
}

/// Escapes the characters freedesktop notification markup gives meaning to.
/// The body (and, on some servers, the summary) is parsed as markup by KDE,
/// dunst, mako and GNOME (`<a>`, `<b>`, `<img>`), so text that came from a
/// pane must reach them as literal text.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Queues `note` for the notification worker: one thread, one session-bus
/// connection reused for every notification, and a queue of four. A full queue
/// drops the note (the rate limit makes that rare); the pane's marks still show
/// the state.
#[cfg(target_os = "linux")]
pub fn send(note: &Note) -> Result<(), String> {
    use std::sync::OnceLock;
    use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

    static WORKER: OnceLock<Option<SyncSender<Note>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = sync_channel::<Note>(4);
        std::thread::Builder::new()
            .name("halyard-notify".into())
            .spawn(move || {
                let mut conn: Option<zbus::blocking::Connection> = None;
                for note in rx {
                    if let Err(e) = send_dbus(&mut conn, &note) {
                        // The next note tries a fresh connection.
                        conn = None;
                        report_failure(&e.to_string());
                    }
                }
            })
            .ok()
            .map(|_| tx)
    });
    let Some(tx) = worker else {
        return Err("could not start the notification thread".into());
    };
    match tx.try_send(note.clone()) {
        Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
        Err(TrySendError::Disconnected(_)) => Err("the notification thread stopped".into()),
    }
}

#[cfg(target_os = "linux")]
fn send_dbus(conn: &mut Option<zbus::blocking::Connection>, note: &Note) -> zbus::Result<()> {
    use std::collections::HashMap;

    use zbus::zvariant::Value;

    if conn.is_none() {
        *conn = Some(zbus::blocking::Connection::session()?);
    }
    let Some(c) = conn.as_ref() else { return Ok(()) };
    let actions: Vec<&str> = Vec::new();
    let hints: HashMap<&str, Value<'_>> = HashMap::new();
    let (title, body) = (escape_markup(&note.title), escape_markup(&note.body));
    c.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "Halyard",
            0u32,
            "utilities-terminal",
            title.as_str(),
            body.as_str(),
            actions,
            hints,
            -1i32,
        ),
    )?;
    Ok(())
}

/// Not supported on this platform.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn send(_note: &Note) -> Result<(), String> {
    Err("desktop notifications are not supported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_escaped_for_the_notification_server() {
        let hostile =
            r#"<a href="https://evil.example/login">Re-authenticate</a> <img src=x> & <b>"#;
        let out = escape_markup(hostile);
        assert_eq!(
            out,
            "&lt;a href=\"https://evil.example/login\"&gt;Re-authenticate&lt;/a&gt; &lt;img src=x&gt; &amp; &lt;b&gt;"
        );
        assert!(!out.contains('<') && !out.contains('>'));
        assert_eq!(escape_markup("plain text"), "plain text");
    }
}

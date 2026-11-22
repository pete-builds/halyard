//! Sending a desktop notification. The decision (when, how often, what text) is
//! [`crate::attention_ui::Notifier`]; this only delivers a [`Note`] whose text is
//! already sanitized.
//!
//! * macOS: `NSUserNotificationCenter` through the Objective-C runtime (no
//!   `osascript`). It needs a bundle identifier, so it works from `Halyard.app`;
//!   a bare binary (`cargo run`) has none and reports that instead. GPUI 0.2.2 has
//!   no notification API.
//! * Linux: the freedesktop `org.freedesktop.Notifications.Notify` call over the
//!   session D-Bus, on a background thread.
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

/// Delivers `note` over D-Bus on a background thread.
#[cfg(target_os = "linux")]
pub fn send(note: &Note) -> Result<(), String> {
    let note = note.clone();
    std::thread::Builder::new()
        .name("halyard-notify".into())
        .spawn(move || {
            if let Err(e) = send_dbus(&note) {
                eprintln!("halyard: desktop notification failed: {e}");
            }
        })
        .map(|_| ())
        .map_err(|e| format!("could not start the notification thread: {e}"))
}

#[cfg(target_os = "linux")]
fn send_dbus(note: &Note) -> zbus::Result<()> {
    use std::collections::HashMap;

    use zbus::zvariant::Value;

    let conn = zbus::blocking::Connection::session()?;
    let actions: Vec<&str> = Vec::new();
    let hints: HashMap<&str, Value<'_>> = HashMap::new();
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "Halyard",
            0u32,
            "utilities-terminal",
            note.title.as_str(),
            note.body.as_str(),
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

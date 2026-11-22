//! Small decisions the pane view makes, kept out of the view so they can be
//! unit tested without a window.

use halyard_term::{KeyCode, MouseButton};

/// What a turn of the mouse wheel does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wheel {
    /// The program tracks the mouse: report wheel presses.
    Report(MouseButton, u32),
    /// Alternate screen without mouse tracking (less, man): arrow keys.
    Arrows(KeyCode, u32),
    /// Normal screen: move the viewport through scrollback by this many
    /// lines (negative is up, into history).
    Scroll(isize),
}

/// Most wheel reports or arrow keys sent for one event.
pub const MAX_WHEEL_REPEAT: u32 = 10;

/// Decide a wheel event of `lines` whole lines; positive means the wheel
/// turned up (toward older output).
#[must_use]
pub fn wheel(lines: i32, mouse_tracking: bool, alt_screen: bool) -> Option<Wheel> {
    if lines == 0 {
        return None;
    }
    let up = lines > 0;
    let count = lines.unsigned_abs().min(MAX_WHEEL_REPEAT);
    Some(if mouse_tracking {
        Wheel::Report(if up { MouseButton::WheelUp } else { MouseButton::WheelDown }, count)
    } else if alt_screen {
        Wheel::Arrows(if up { KeyCode::Up } else { KeyCode::Down }, count)
    } else {
        // Wheel up moves into history, which is a negative scroll.
        Wheel::Scroll(-(lines as isize))
    })
}

/// Whether a program's exit closes its pane at once. Only a clean exit
/// does; any other status leaves the pane open with a notice.
#[must_use]
pub fn exit_closes_pane(code: Option<i32>) -> bool {
    code == Some(0)
}

/// Whether to ask before pasting `text`: the setting is on, the program has
/// not enabled bracketed paste (so a newline would run a command), and the
/// text is not safe (holds a line break or a paste-end marker).
#[must_use]
pub fn paste_needs_confirmation(setting: bool, bracketed: bool, text: &str) -> bool {
    setting && !bracketed && !halyard_term::paste_is_safe(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_up_scrolls_into_history() {
        assert_eq!(wheel(3, false, false), Some(Wheel::Scroll(-3)));
        assert_eq!(wheel(-2, false, false), Some(Wheel::Scroll(2)));
        assert_eq!(wheel(0, false, false), None);
    }

    #[test]
    fn wheel_reports_and_arrows_keep_direction() {
        assert_eq!(wheel(2, true, false), Some(Wheel::Report(MouseButton::WheelUp, 2)));
        assert_eq!(wheel(-2, true, true), Some(Wheel::Report(MouseButton::WheelDown, 2)));
        assert_eq!(wheel(1, false, true), Some(Wheel::Arrows(KeyCode::Up, 1)));
        assert_eq!(wheel(-50, false, true), Some(Wheel::Arrows(KeyCode::Down, 10)));
    }

    #[test]
    fn only_a_clean_exit_closes_the_pane() {
        assert!(exit_closes_pane(Some(0)));
        assert!(!exit_closes_pane(Some(1)));
        assert!(!exit_closes_pane(Some(130)));
        assert!(!exit_closes_pane(None));
    }

    #[test]
    fn multiline_paste_asks_only_without_bracketed_paste() {
        assert!(paste_needs_confirmation(true, false, "ls\nrm -rf x\n"));
        assert!(
            !paste_needs_confirmation(true, true, "ls\nrm\n"),
            "bracketed: shell will not run it"
        );
        assert!(!paste_needs_confirmation(false, false, "ls\nrm\n"), "setting off");
        assert!(!paste_needs_confirmation(true, false, "one line"), "safe text");
    }
}

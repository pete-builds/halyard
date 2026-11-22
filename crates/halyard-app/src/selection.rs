//! Where a selection is copied, and what a middle click does.
//!
//! Linux has two clipboards: CLIPBOARD (explicit copy and paste) and PRIMARY
//! (whatever was last selected, pasted with the middle button). Selecting
//! text writes PRIMARY there, as every X11 and Wayland terminal does.
//! `terminal.copy_on_select` also writes CLIPBOARD, on every platform.

/// Which clipboards a finished selection is written to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyTargets {
    /// The regular clipboard.
    pub clipboard: bool,
    /// The Linux PRIMARY selection.
    pub primary: bool,
}

/// Targets for a selection the user just made.
#[must_use]
pub fn on_selection(linux: bool, copy_on_select: bool) -> CopyTargets {
    CopyTargets { clipboard: copy_on_select, primary: linux }
}

/// What a middle-button press does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiddleClick {
    /// Paste PRIMARY through the normal (sanitized, bracketed) paste path.
    PastePrimary,
    /// The program tracks the mouse: report the click to it.
    SendToApp,
    /// Nothing.
    Ignore,
}

/// Decide a middle click. Shift overrides mouse tracking, as it does for
/// selecting with the left button.
#[must_use]
pub fn middle_click(linux: bool, mouse_tracking: bool, shift: bool) -> MiddleClick {
    if mouse_tracking && !shift {
        MiddleClick::SendToApp
    } else if linux {
        MiddleClick::PastePrimary
    } else {
        MiddleClick::Ignore
    }
}

/// True when this build is for Linux (where PRIMARY exists).
pub const LINUX: bool = cfg!(target_os = "linux");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_targets() {
        assert_eq!(on_selection(true, false), CopyTargets { clipboard: false, primary: true });
        assert_eq!(on_selection(true, true), CopyTargets { clipboard: true, primary: true });
        assert_eq!(on_selection(false, false), CopyTargets { clipboard: false, primary: false });
        assert_eq!(on_selection(false, true), CopyTargets { clipboard: true, primary: false });
    }

    #[test]
    fn middle_click_rules() {
        assert_eq!(middle_click(true, false, false), MiddleClick::PastePrimary);
        assert_eq!(middle_click(true, true, false), MiddleClick::SendToApp);
        assert_eq!(middle_click(true, true, true), MiddleClick::PastePrimary);
        assert_eq!(middle_click(false, false, false), MiddleClick::Ignore);
        assert_eq!(middle_click(false, true, false), MiddleClick::SendToApp);
    }
}

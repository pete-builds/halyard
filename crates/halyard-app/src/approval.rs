//! The approval dialogs' state and key handling, without any drawing.
//!
//! A dialog appears while the user may be typing into a pane, so nothing typed by
//! habit may answer it with a yes:
//!
//! * plain Enter never approves, at any time;
//! * nothing approves during the first [`ARM_DELAY`] after the dialog opens, so a
//!   key already on its way (or a double click) lands on nothing;
//! * approving takes a deliberate chord (Cmd+Enter on macOS, Ctrl+Shift+Enter
//!   elsewhere) or a click on the Approve button once armed;
//! * Escape denies at any time, since denying is always safe;
//! * every other key is swallowed by the dialog, so it does not reach the pane.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use halyard_ipc::{PairingDecision, PairingRequest, Scope, SendTextRequest};

/// How long a new dialog ignores approving keys and clicks.
pub const ARM_DELAY: Duration = Duration::from_millis(700);

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
    /// The user approved.
    Approve,
    /// The user denied.
    Deny,
}

/// The chord that approves, shown on the button.
#[must_use]
pub fn approve_chord_label(macos: bool) -> &'static str {
    if macos { "⌘⏎" } else { "Ctrl+Shift+Enter" }
}

/// Whether this key press is the approve chord on this platform.
#[must_use]
pub fn is_approve_chord(key: &str, m: Mods, macos: bool) -> bool {
    key == "enter"
        && !m.alt
        && if macos { m.platform && !m.control } else { m.control && m.shift && !m.platform }
}

/// One open dialog.
#[derive(Debug, Clone)]
pub struct Dialog {
    /// The question.
    pub ask: Ask,
    opened: Instant,
    /// Pairing: the scopes currently ticked, a subset of those requested.
    pub scopes: BTreeSet<Scope>,
    /// Pairing: "always allow typing from this tool" is ticked.
    pub always_allow_input: bool,
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
        Self { ask, opened: now, scopes, always_allow_input: false }
    }

    /// Whether approving is possible yet.
    #[must_use]
    pub fn armed(&self, now: Instant) -> bool {
        now.duration_since(self.opened) >= ARM_DELAY
    }

    /// When the dialog arms.
    #[must_use]
    pub fn arms_at(&self) -> Instant {
        self.opened + ARM_DELAY
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

    /// Ticks or unticks "always allow typing", when offered.
    pub fn toggle_always(&mut self) -> Outcome {
        if !self.offers_always() {
            return Outcome::Ignored;
        }
        self.always_allow_input = !self.always_allow_input;
        Outcome::Changed
    }

    /// A key press at `now`.
    pub fn key(&mut self, key: &str, m: Mods, macos: bool, now: Instant) -> Outcome {
        if key == "escape" {
            return Outcome::Deny;
        }
        if !self.armed(now) {
            return Outcome::Ignored;
        }
        if is_approve_chord(key, m, macos) {
            return self.approve();
        }
        if m.control || m.platform || m.alt {
            return Outcome::Ignored;
        }
        if let Some(n) = key.parse::<usize>().ok().filter(|n| *n >= 1) {
            return match self.requested().get(n - 1) {
                Some(scope) => self.toggle_scope(*scope),
                None => Outcome::Ignored,
            };
        }
        if key == "t" {
            return self.toggle_always();
        }
        Outcome::Ignored
    }

    /// A click on Approve at `now`.
    pub fn click_approve(&mut self, now: Instant) -> Outcome {
        if self.armed(now) { self.approve() } else { Outcome::Ignored }
    }

    fn approve(&self) -> Outcome {
        // A pairing with nothing ticked would be refused by the server anyway.
        if matches!(self.ask, Ask::Pairing(_)) && self.scopes.is_empty() {
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
    use halyard_ipc::PeerInfo;

    fn pairing(scopes: &[Scope]) -> Dialog {
        Dialog::new(
            Ask::Pairing(PairingRequest {
                client_name: "tool".into(),
                scopes: scopes.iter().copied().collect(),
                peer: PeerInfo { pid: 1, uid: 1, exe: None },
            }),
            Instant::now(),
        )
    }

    const CMD: Mods = Mods { control: false, alt: false, shift: false, platform: true };
    const NONE: Mods = Mods { control: false, alt: false, shift: false, platform: false };
    const CTRL_SHIFT: Mods = Mods { control: true, alt: false, shift: true, platform: false };

    #[test]
    fn plain_enter_never_approves() {
        let mut d = pairing(&[Scope::SessionsRead]);
        let later = d.arms_at() + Duration::from_secs(5);
        assert_eq!(d.key("enter", NONE, true, later), Outcome::Ignored);
        assert_eq!(d.key("enter", NONE, false, later), Outcome::Ignored);
        // Shift+Enter is what a fast typist hits by accident: not enough either.
        let shift = Mods { shift: true, ..NONE };
        assert_eq!(d.key("enter", shift, true, later), Outcome::Ignored);
    }

    #[test]
    fn the_approve_chord_works_only_once_armed() {
        let mut d = pairing(&[Scope::SessionsRead]);
        let early = d.arms_at() - Duration::from_millis(1);
        assert_eq!(d.key("enter", CMD, true, early), Outcome::Ignored);
        assert_eq!(d.click_approve(early), Outcome::Ignored);
        let armed = d.arms_at();
        assert_eq!(d.key("enter", CMD, true, armed), Outcome::Approve);
        assert_eq!(d.click_approve(armed), Outcome::Approve);
        // The chord is per platform.
        assert_eq!(d.key("enter", CTRL_SHIFT, false, armed), Outcome::Approve);
        assert_eq!(d.key("enter", CMD, false, armed), Outcome::Ignored);
        assert_eq!(d.key("enter", CTRL_SHIFT, true, armed), Outcome::Ignored);
    }

    #[test]
    fn escape_denies_at_once() {
        let mut d = pairing(&[Scope::SessionsRead]);
        assert_eq!(d.key("escape", NONE, true, Instant::now()), Outcome::Deny);
    }

    #[test]
    fn scopes_toggle_by_number_and_always_needs_input_write() {
        let mut d = pairing(&[Scope::SessionsRead, Scope::InputWrite]);
        let t = d.arms_at();
        assert!(d.offers_always());
        assert_eq!(d.key("t", NONE, true, t), Outcome::Changed);
        assert!(d.always_allow_input);
        // InputWrite is the second requested scope (BTreeSet order).
        assert_eq!(d.requested(), vec![Scope::SessionsRead, Scope::InputWrite]);
        assert_eq!(d.key("2", NONE, true, t), Outcome::Changed);
        assert!(!d.scopes.contains(&Scope::InputWrite));
        assert!(!d.always_allow_input, "unticking input:write drops always-allow");
        assert_eq!(d.key("t", NONE, true, t), Outcome::Ignored);
        assert_eq!(d.key("9", NONE, true, t), Outcome::Ignored);
        assert_eq!(
            d.pairing_decision(),
            PairingDecision::Approve {
                scopes: [Scope::SessionsRead].into_iter().collect(),
                always_allow_input: false
            }
        );
        // Nothing ticked: approving does nothing.
        assert_eq!(d.key("1", NONE, true, t), Outcome::Changed);
        assert_eq!(d.click_approve(t), Outcome::Ignored);
    }

    #[test]
    fn letters_typed_into_the_dialog_are_swallowed() {
        let mut d = pairing(&[Scope::SessionsRead]);
        let t = d.arms_at();
        for k in ["y", "a", "space", "tab", "c"] {
            assert_eq!(d.key(k, NONE, true, t), Outcome::Ignored, "{k}");
        }
        assert_eq!(d.key("c", Mods { control: true, ..NONE }, true, t), Outcome::Ignored);
    }
}

//! One window's tabs, each a split tree, and the operations on them.

use crate::tree::{Axis, Direction, MinSize, Node, PaneId, Rect};

/// One tab: a split tree, the pane with focus, and an optional name.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    /// The panes.
    pub root: Node,
    /// The pane with keyboard focus. Always a pane of `root`.
    pub focused: PaneId,
    /// Focus mode: only the focused pane is shown, filling the tab.
    pub zoomed: bool,
    /// A name the user gave the tab. `None` shows the focused pane's title.
    pub name: Option<String>,
}

impl Tab {
    fn single(id: PaneId) -> Self {
        Self { root: Node::Pane(id), focused: id, zoomed: false, name: None }
    }

    /// Each visible pane's rectangle: all panes, or only the focused one
    /// filling `bounds` when zoomed.
    #[must_use]
    pub fn layout(&self, bounds: Rect, gap: f32) -> Vec<(PaneId, Rect)> {
        if self.zoomed { vec![(self.focused, bounds)] } else { self.root.layout(bounds, gap) }
    }

    /// Dividers to draw and drag (none while zoomed).
    #[must_use]
    pub fn dividers(&self, bounds: Rect, gap: f32) -> Vec<crate::tree::Divider> {
        if self.zoomed { Vec::new() } else { self.root.dividers(bounds, gap) }
    }
}

/// What closing a pane did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// The pane was removed; its tab lives on.
    Pane,
    /// It was the tab's last pane, so the tab closed.
    Tab,
    /// It was the window's last pane: no tabs remain, close the window.
    Window,
    /// No such pane.
    NotFound,
}

/// The longest tab name kept, in characters.
pub const MAX_TAB_NAME: usize = 256;

/// One window's tabs.
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
}

impl Workspace {
    /// A window with one tab holding one pane.
    #[must_use]
    pub fn new() -> (Self, PaneId) {
        let id = PaneId(1);
        (Self { tabs: vec![Tab::single(id)], active: 0, next_id: 2 }, id)
    }

    /// Build from tabs (used when restoring). `tabs` must not be empty and
    /// every pane id must be unique; ids are renumbered from 1.
    #[must_use]
    pub(crate) fn from_tabs(tabs: Vec<Tab>, active: usize) -> Option<Self> {
        if tabs.is_empty() {
            return None;
        }
        let next = tabs.iter().flat_map(|t| t.root.panes()).map(|p| p.0).max().unwrap_or(0) + 1;
        let active = active.min(tabs.len() - 1);
        let ws = Self { tabs, active, next_id: next };
        ws.check().ok()?;
        Some(ws)
    }

    fn alloc(&mut self) -> PaneId {
        let id = PaneId(self.next_id);
        self.next_id += 1;
        id
    }

    /// The tabs, in order.
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Index of the active tab.
    #[must_use]
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The active tab.
    ///
    /// # Panics
    /// When the window has no tabs left (after [`Closed::Window`]).
    #[must_use]
    pub fn active(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// True when every tab has closed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The focused pane of the active tab.
    #[must_use]
    pub fn focused(&self) -> Option<PaneId> {
        self.tabs.get(self.active).map(|t| t.focused)
    }

    /// Every pane in every tab.
    #[must_use]
    pub fn all_panes(&self) -> Vec<PaneId> {
        self.tabs.iter().flat_map(|t| t.root.panes()).collect()
    }

    /// Which tab holds a pane.
    #[must_use]
    pub fn tab_of(&self, id: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.path_to(id).is_some())
    }

    /// Open a tab after the active one and make it active. Returns its pane.
    pub fn new_tab(&mut self) -> PaneId {
        let id = self.alloc();
        let at = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        self.tabs.insert(at, Tab::single(id));
        self.active = at;
        id
    }

    /// Split the focused pane: `Horizontal` puts the new pane to the right,
    /// `Vertical` below. The new pane gets focus. Leaves focus mode.
    pub fn split(&mut self, axis: Axis) -> Option<PaneId> {
        let id = self.alloc();
        let tab = self.tabs.get_mut(self.active)?;
        tab.zoomed = false;
        tab.root.split_pane(tab.focused, id, axis).then(|| {
            tab.focused = id;
            id
        })
    }

    /// Close a pane in any tab. Its sibling absorbs its space and the
    /// nearest pane takes focus.
    pub fn close_pane(&mut self, id: PaneId) -> Closed {
        let Some(t) = self.tab_of(id) else { return Closed::NotFound };
        let tab = &mut self.tabs[t];
        if tab.root.len() > 1 {
            let next = tab.root.remove_pane(id);
            if tab.focused == id
                && let Some(n) = next
            {
                tab.focused = n;
            }
            if tab.root.len() == 1 {
                tab.zoomed = false;
            }
            return Closed::Pane;
        }
        self.tabs.remove(t);
        if self.tabs.is_empty() {
            self.active = 0;
            return Closed::Window;
        }
        if t < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        Closed::Tab
    }

    /// Focus a pane (and its tab). Returns false if it is not here.
    pub fn focus_pane(&mut self, id: PaneId) -> bool {
        let Some(t) = self.tab_of(id) else { return false };
        self.active = t;
        let tab = &mut self.tabs[t];
        if tab.zoomed && tab.focused != id {
            tab.zoomed = false;
        }
        tab.focused = id;
        true
    }

    /// Move focus to the neighboring pane in `dir`. Leaves focus mode.
    pub fn focus_direction(&mut self, dir: Direction, bounds: Rect, gap: f32) -> Option<PaneId> {
        let tab = self.tabs.get_mut(self.active)?;
        let next = tab.root.neighbor(tab.focused, dir, bounds, gap)?;
        tab.zoomed = false;
        tab.focused = next;
        Some(next)
    }

    /// Move the focused pane's divider toward `dir` by `step`.
    pub fn resize_focused(
        &mut self,
        dir: Direction,
        step: f32,
        bounds: Rect,
        gap: f32,
        min: MinSize,
    ) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else { return false };
        if tab.zoomed {
            return false;
        }
        let f = tab.focused;
        tab.root.resize_pane(f, dir, step, bounds, gap, min)
    }

    /// Drag a divider of the active tab (see [`Node::drag_divider`]).
    pub fn drag_divider(
        &mut self,
        path: &[bool],
        pos: f32,
        bounds: Rect,
        gap: f32,
        min: MinSize,
    ) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else { return false };
        tab.root.drag_divider(path, pos, bounds, gap, min)
    }

    /// Toggle focus mode in the active tab. A tab with one pane never zooms.
    pub fn toggle_zoom(&mut self) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else { return false };
        if tab.root.len() < 2 && !tab.zoomed {
            return false;
        }
        tab.zoomed = !tab.zoomed;
        true
    }

    /// Activate a tab by index.
    pub fn select_tab(&mut self, index: usize) -> bool {
        if index < self.tabs.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// Activate the next tab, wrapping.
    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    /// Activate the previous tab, wrapping.
    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    /// Move a tab to another position; the active tab stays active.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return false;
        }
        let active_pane = self.tabs[self.active].focused;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.tab_of(active_pane).unwrap_or(0);
        true
    }

    /// Name a tab, or clear its name with `None` or a blank string. Control
    /// characters are dropped and the name is cut at [`MAX_TAB_NAME`].
    pub fn rename_tab(&mut self, index: usize, name: Option<&str>) -> bool {
        let Some(tab) = self.tabs.get_mut(index) else { return false };
        tab.name = name.map(clean_name).filter(|n| !n.is_empty());
        true
    }

    /// Check the invariants: at least one tab (unless all closed), a valid
    /// active index, focus on a pane of its own tab, unique pane ids below
    /// the id counter, ratios in range.
    ///
    /// # Errors
    /// A description of the first broken invariant.
    pub fn check(&self) -> Result<(), String> {
        if !self.tabs.is_empty() && self.active >= self.tabs.len() {
            return Err(format!("active tab {} of {}", self.active, self.tabs.len()));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, t) in self.tabs.iter().enumerate() {
            if t.root.path_to(t.focused).is_none() {
                return Err(format!("tab {i}: focused pane not in tab"));
            }
            for p in t.root.panes() {
                if !seen.insert(p) {
                    return Err(format!("pane {p:?} appears twice"));
                }
                if p.0 >= self.next_id {
                    return Err(format!("pane {p:?} not below the id counter"));
                }
            }
            for r in t.root.ratios() {
                if !(crate::tree::RATIO_LIMITS.0..=crate::tree::RATIO_LIMITS.1).contains(&r) {
                    return Err(format!("tab {i}: ratio {r} out of range"));
                }
            }
        }
        Ok(())
    }
}

/// A tab name with control characters removed, trimmed, and cut to
/// [`MAX_TAB_NAME`] characters.
#[must_use]
pub fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_TAB_NAME).collect::<String>().trim().to_owned()
}

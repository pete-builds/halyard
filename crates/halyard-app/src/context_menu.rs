//! Right-click menus for tabs and panes, without any drawing: which items a
//! menu holds, which are enabled, keyboard selection, where the menu goes,
//! and whether a right click in a pane opens it at all.

use halyard_config::Action;

/// Width of a menu.
pub const MENU_W: f32 = 220.0;
/// Height of one item row.
pub const ROW_H: f32 = 24.0;
/// Height of a separator.
pub const SEP_H: f32 = 9.0;
/// Space above the first and below the last row.
pub const PAD_Y: f32 = 4.0;
/// Closest a menu comes to the window edge.
pub const EDGE: f32 = 4.0;

/// Something a menu item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// Edit the tab's name in place.
    RenameTab,
    /// Close the tab (asks first when a command is running).
    CloseTab,
    /// Close every other tab (asks first when a command is running).
    CloseOtherTabs,
    /// Move the tab one place left.
    MoveTabLeft,
    /// Move the tab one place right.
    MoveTabRight,
    /// Open a new tab.
    NewTab,
    /// Copy the selection.
    Copy,
    /// Paste the clipboard.
    Paste,
    /// Select all of scrollback and the screen.
    SelectAll,
    /// Split the pane to the right.
    SplitRight,
    /// Split the pane downward.
    SplitDown,
    /// Close the pane (asks first when a command is running).
    ClosePane,
}

impl Item {
    /// The label shown in the menu.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Item::RenameTab => "Rename Tab",
            Item::CloseTab => "Close Tab",
            Item::CloseOtherTabs => "Close Other Tabs",
            Item::MoveTabLeft => "Move Tab Left",
            Item::MoveTabRight => "Move Tab Right",
            Item::NewTab => "New Tab",
            Item::Copy => "Copy",
            Item::Paste => "Paste",
            Item::SelectAll => "Select All",
            Item::SplitRight => "Split Right",
            Item::SplitDown => "Split Down",
            Item::ClosePane => "Close Pane",
        }
    }

    /// The keymap action with the same effect, whose chord the menu shows.
    #[must_use]
    pub fn action(self) -> Option<Action> {
        Some(match self {
            Item::RenameTab => Action::RenameTab,
            Item::MoveTabLeft => Action::MoveTabLeft,
            Item::MoveTabRight => Action::MoveTabRight,
            Item::NewTab => Action::NewTab,
            Item::Copy => Action::Copy,
            Item::Paste => Action::Paste,
            Item::SplitRight => Action::SplitRight,
            Item::SplitDown => Action::SplitDown,
            Item::ClosePane => Action::ClosePane,
            Item::CloseTab | Item::CloseOtherTabs | Item::SelectAll => return None,
        })
    }
}

/// One row of a menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// What it does.
    pub item: Item,
    /// Whether it can be chosen now.
    pub enabled: bool,
    /// Draw a separator above it.
    pub separator: bool,
}

const fn entry(item: Item, enabled: bool, separator: bool) -> Entry {
    Entry { item, enabled, separator }
}

/// The menu for tab `index` of `count` tabs.
#[must_use]
pub fn tab_entries(index: usize, count: usize) -> Vec<Entry> {
    vec![
        entry(Item::RenameTab, true, false),
        entry(Item::CloseTab, true, true),
        entry(Item::CloseOtherTabs, count > 1, false),
        entry(Item::MoveTabLeft, index > 0, true),
        entry(Item::MoveTabRight, index + 1 < count, false),
        entry(Item::NewTab, true, true),
    ]
}

/// The menu for a pane. Copy needs a selection; splits need the layout to
/// allow one more pane.
#[must_use]
pub fn pane_entries(has_selection: bool, can_split: bool) -> Vec<Entry> {
    vec![
        entry(Item::Copy, has_selection, false),
        entry(Item::Paste, true, false),
        entry(Item::SelectAll, true, false),
        entry(Item::SplitRight, can_split, true),
        entry(Item::SplitDown, can_split, false),
        entry(Item::ClosePane, true, true),
    ]
}

/// Whether a right click in a pane opens the menu (true) or goes to the
/// program (false). A program that turned on mouse reporting (vim, htop,
/// tmux) gets a plain right click; Shift always opens the menu.
#[must_use]
pub fn right_click_opens_menu(mouse_tracking: bool, shift: bool) -> bool {
    !mouse_tracking || shift
}

/// A menu's height for these entries.
#[must_use]
pub fn height(entries: &[Entry]) -> f32 {
    let seps = entries.iter().filter(|e| e.separator).count();
    // A handful of rows: the counts convert exactly.
    PAD_Y * 2.0 + ROW_H * entries.len() as f32 + SEP_H * seps as f32
}

/// The top left corner for a menu of `w` by `h` opened at (`x`, `y`) in a
/// window `vw` by `vh`: at the pointer when it fits, else moved left or up
/// to stay inside the window, never closer than [`EDGE`] to an edge (unless
/// the window is smaller than the menu, when it sits at the top left).
#[must_use]
pub fn place(x: f32, y: f32, w: f32, h: f32, vw: f32, vh: f32) -> (f32, f32) {
    let fit = |p: f32, size: f32, room: f32| {
        p.min(room - size - EDGE).max(EDGE.min(room - size).max(0.0))
    };
    (fit(x, w, vw), fit(y, h, vh))
}

/// A menu's keyboard and pointer selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    /// The rows.
    pub entries: Vec<Entry>,
    selected: Option<usize>,
}

impl Menu {
    /// A menu with nothing selected (a right click selects nothing; the
    /// first arrow key selects the first enabled row).
    #[must_use]
    pub fn new(entries: Vec<Entry>) -> Self {
        Self { entries, selected: None }
    }

    /// The selected row.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Select row `i` if it is enabled (the pointer moved over it).
    pub fn hover(&mut self, i: usize) {
        if self.entries.get(i).is_some_and(|e| e.enabled) {
            self.selected = Some(i);
        }
    }

    /// Move the selection by one row down (`down`) or up, skipping disabled
    /// rows and wrapping. With nothing selected, Down picks the first enabled
    /// row and Up the last.
    pub fn step(&mut self, down: bool) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let mut i = match (self.selected, down) {
            (Some(i), _) => i,
            (None, true) => n - 1,
            (None, false) => 0,
        };
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if self.entries[i].enabled {
                self.selected = Some(i);
                return;
            }
        }
    }

    /// The item Enter would run: the selected row, when it is enabled.
    #[must_use]
    pub fn chosen(&self) -> Option<Item> {
        self.selected.and_then(|i| self.entries.get(i)).filter(|e| e.enabled).map(|e| e.item)
    }

    /// The item a click on row `i` runs, when it is enabled.
    #[must_use]
    pub fn clicked(&self, i: usize) -> Option<Item> {
        self.entries.get(i).filter(|e| e.enabled).map(|e| e.item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(entries: &[Entry], item: Item) -> bool {
        entries.iter().find(|e| e.item == item).is_some_and(|e| e.enabled)
    }

    #[test]
    fn right_click_goes_to_programs_that_report_the_mouse() {
        assert!(right_click_opens_menu(false, false), "no reporting: menu");
        assert!(right_click_opens_menu(false, true), "no reporting, Shift: menu");
        assert!(!right_click_opens_menu(true, false), "reporting: the program gets it");
        assert!(right_click_opens_menu(true, true), "reporting, Shift: menu");
    }

    #[test]
    fn tab_menu_items_and_their_state() {
        let items: Vec<Item> = tab_entries(0, 3).iter().map(|e| e.item).collect();
        assert_eq!(
            items,
            [
                Item::RenameTab,
                Item::CloseTab,
                Item::CloseOtherTabs,
                Item::MoveTabLeft,
                Item::MoveTabRight,
                Item::NewTab
            ]
        );
        let first = tab_entries(0, 3);
        assert!(!enabled(&first, Item::MoveTabLeft), "first tab cannot move left");
        assert!(enabled(&first, Item::MoveTabRight));
        let last = tab_entries(2, 3);
        assert!(enabled(&last, Item::MoveTabLeft));
        assert!(!enabled(&last, Item::MoveTabRight), "last tab cannot move right");
        let only = tab_entries(0, 1);
        assert!(!enabled(&only, Item::CloseOtherTabs), "no other tabs");
        assert!(!enabled(&only, Item::MoveTabLeft) && !enabled(&only, Item::MoveTabRight));
        assert!(enabled(&only, Item::RenameTab) && enabled(&only, Item::CloseTab));
        assert!(enabled(&only, Item::NewTab));
    }

    #[test]
    fn pane_menu_items_and_their_state() {
        let items: Vec<Item> = pane_entries(true, true).iter().map(|e| e.item).collect();
        assert_eq!(
            items,
            [
                Item::Copy,
                Item::Paste,
                Item::SelectAll,
                Item::SplitRight,
                Item::SplitDown,
                Item::ClosePane
            ]
        );
        assert!(!enabled(&pane_entries(false, true), Item::Copy), "Copy needs a selection");
        assert!(enabled(&pane_entries(true, true), Item::Copy));
        let full = pane_entries(false, false);
        assert!(!enabled(&full, Item::SplitRight) && !enabled(&full, Item::SplitDown));
        assert!(enabled(&full, Item::Paste) && enabled(&full, Item::ClosePane));
        assert!(enabled(&full, Item::SelectAll));
    }

    #[test]
    fn keyboard_selection_skips_disabled_rows_and_wraps() {
        // Copy is disabled: Down starts at Paste, Up from Paste wraps to Close Pane.
        let mut m = Menu::new(pane_entries(false, true));
        assert_eq!(m.chosen(), None, "nothing selected yet");
        m.step(true);
        assert_eq!(m.chosen(), Some(Item::Paste));
        m.step(false);
        assert_eq!(m.chosen(), Some(Item::ClosePane), "skipped disabled Copy and wrapped");
        m.step(true);
        assert_eq!(m.chosen(), Some(Item::Paste), "wrapped back past Copy");
        let mut m = Menu::new(pane_entries(true, true));
        m.step(false);
        assert_eq!(m.chosen(), Some(Item::ClosePane), "Up with nothing selected: the last");
        // Nothing enabled: nothing to select.
        let mut m = Menu::new(vec![entry(Item::Copy, false, false)]);
        m.step(true);
        assert_eq!(m.selected(), None);
    }

    #[test]
    fn disabled_rows_cannot_be_hovered_or_clicked() {
        let mut m = Menu::new(pane_entries(false, true));
        m.hover(0);
        assert_eq!(m.selected(), None);
        assert_eq!(m.clicked(0), None);
        assert_eq!(m.clicked(1), Some(Item::Paste));
        assert_eq!(m.clicked(99), None);
        m.hover(3);
        assert_eq!(m.chosen(), Some(Item::SplitRight));
    }

    #[test]
    fn menus_stay_inside_the_window() {
        let (w, h) = (MENU_W, 200.0);
        assert_eq!(place(100.0, 50.0, w, h, 800.0, 600.0), (100.0, 50.0), "fits at the pointer");
        assert_eq!(place(700.0, 50.0, w, h, 800.0, 600.0), (800.0 - w - EDGE, 50.0));
        assert_eq!(place(100.0, 550.0, w, h, 800.0, 600.0), (100.0, 600.0 - h - EDGE));
        assert_eq!(place(-5.0, -5.0, w, h, 800.0, 600.0), (EDGE, EDGE));
        // A window smaller than the menu: pinned to the top left, never negative.
        assert_eq!(place(50.0, 50.0, w, h, 100.0, 100.0), (0.0, 0.0));
    }

    #[test]
    fn height_counts_rows_and_separators() {
        let e = tab_entries(0, 2);
        assert_eq!(height(&e), PAD_Y * 2.0 + ROW_H * 6.0 + SEP_H * 3.0);
    }

    #[test]
    fn every_item_with_an_action_names_a_real_one() {
        for item in [Item::RenameTab, Item::Copy, Item::Paste, Item::SplitRight, Item::ClosePane] {
            let a = item.action().unwrap();
            assert!(crate::keybind::gpui_action(a).is_some(), "{}", a.name());
        }
    }
}

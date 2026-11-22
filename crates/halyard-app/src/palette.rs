//! The command palette's state, without any drawing: fuzzy filtering, the
//! selection, and the three modes (commands, theme picker, tab rename).

use halyard_config::Action;

/// Score how well `query` matches `candidate`, or `None` when it does not
/// (every query character must appear in order, ignoring case). Higher is
/// better: consecutive matches and matches at word starts count extra, gaps
/// cost a little, and an empty query matches everything equally.
#[must_use]
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    let cand: Vec<char> = candidate.chars().collect();
    let mut score = 0i32;
    let mut pos = 0usize;
    let mut prev: Option<usize> = None;
    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let ql = q.to_lowercase().next().unwrap_or(q);
        let found = (pos..cand.len()).find(|&i| cand[i].to_lowercase().next() == Some(ql))?;
        score += 10;
        if prev.is_some_and(|p| p + 1 == found) {
            score += 15;
        }
        let word_start = found == 0 || matches!(cand[found - 1], ' ' | '_' | '-' | '/' | '.');
        if word_start {
            score += 20;
        }
        let gap = found - prev.map_or(0, |p| p + 1);
        score -= i32::try_from(gap).unwrap_or(i32::MAX).min(20);
        prev = Some(found);
        pos = found + 1;
    }
    Some(score)
}

/// Indexes of `items` matching `query`, best first; ties keep list order.
#[must_use]
pub fn filter(query: &str, items: &[String]) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| fuzzy_score(query, s).map(|sc| (sc, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Something the palette can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// A keymap action.
    Run(Action),
    /// Open the theme picker.
    SwitchTheme,
    /// Write the shell integration script and copy the line that loads it.
    CopyShellIntegration,
    /// List the tools paired through the session API, and revoke them.
    PairedTools,
}

/// The palette title for an action, or `None` for actions not offered
/// (the palette itself, and actions not built yet).
#[must_use]
pub fn title(a: Action) -> Option<&'static str> {
    Some(match a {
        Action::NewTab => "New Tab",
        Action::ClosePane => "Close Pane",
        Action::SplitRight => "Split Right",
        Action::SplitDown => "Split Down",
        Action::FocusLeft => "Focus Pane Left",
        Action::FocusRight => "Focus Pane Right",
        Action::FocusUp => "Focus Pane Up",
        Action::FocusDown => "Focus Pane Down",
        Action::NextTab => "Next Tab",
        Action::PrevTab => "Previous Tab",
        Action::Copy => "Copy",
        Action::Paste => "Paste",
        Action::ZoomPane => "Toggle Pane Zoom",
        Action::IncreaseFontSize => "Increase Font Size",
        Action::DecreaseFontSize => "Decrease Font Size",
        Action::ResetFontSize => "Reset Font Size",
        Action::NewWindow => "New Window",
        Action::Quit => "Quit Halyard",
        Action::ResizePaneLeft => "Resize Pane Left",
        Action::ResizePaneRight => "Resize Pane Right",
        Action::ResizePaneUp => "Resize Pane Up",
        Action::ResizePaneDown => "Resize Pane Down",
        Action::MoveTabLeft => "Move Tab Left",
        Action::MoveTabRight => "Move Tab Right",
        Action::RenameTab => "Rename Tab",
        Action::ReloadConfig => "Reload Config",
        Action::OpenConfig => "Open Config File",
        Action::AgentDashboard => "Agent Dashboard",
        Action::PrevPrompt => "Jump to Previous Prompt",
        Action::NextPrompt => "Jump to Next Prompt",
        Action::CopyLastOutput => "Copy Last Command Output",
        Action::CommandPalette | Action::Find => return None,
    })
}

/// Every palette command with its title, in a stable order.
#[must_use]
pub fn commands() -> Vec<(Command, &'static str)> {
    let mut v: Vec<(Command, &'static str)> =
        Action::ALL.iter().filter_map(|a| title(*a).map(|t| (Command::Run(*a), t))).collect();
    v.insert(0, (Command::SwitchTheme, "Switch Theme"));
    v.insert(1, (Command::CopyShellIntegration, "Copy Shell Integration Line"));
    v.insert(2, (Command::PairedTools, "Paired Tools (Session API)"));
    v
}

/// Whether an action from the palette goes to the focused pane (it is
/// handled there, not by the window) once the palette closes.
#[must_use]
pub fn runs_in_pane(a: Action) -> bool {
    matches!(a, Action::Copy | Action::Paste)
}

/// Whether a theme preview may stay on screen when `next` is the palette
/// now showing (`None`: no palette). Only the theme picker keeps it.
#[must_use]
pub fn preview_survives(next: Option<&Mode>) -> bool {
    matches!(next, Some(Mode::Themes { .. }))
}

/// What the palette is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Listing commands.
    Commands,
    /// Picking a theme. `previous` is the override to restore on Escape.
    Themes {
        /// The theme override in effect before the picker opened.
        previous: Option<String>,
    },
    /// Renaming a tab; the query is the new name.
    Rename {
        /// Tab index.
        tab: usize,
    },
    /// The agent dashboard: one entry per pane, in attention order.
    Agents,
    /// Tools paired through the session API; Enter revokes the selected one.
    PairedTools,
}

/// Palette state: mode, query, entries and selection.
#[derive(Clone, Debug)]
pub struct Model {
    /// Current mode.
    pub mode: Mode,
    /// Typed text.
    pub query: String,
    entries: Vec<String>,
    matches: Vec<usize>,
    selected: usize,
}

impl Model {
    /// A palette in `mode` over `entries` (empty for rename), with an
    /// initial query and the entry to select first (if it matches).
    #[must_use]
    pub fn new(mode: Mode, entries: Vec<String>, query: String, select: Option<usize>) -> Self {
        let mut m = Self { mode, query, entries, matches: Vec::new(), selected: 0 };
        m.refilter();
        if let Some(i) = select.and_then(|e| m.matches.iter().position(|x| *x == e)) {
            m.selected = i;
        }
        m
    }

    fn refilter(&mut self) {
        self.matches = filter(&self.query, &self.entries);
        self.selected = 0;
    }

    /// Add typed text.
    pub fn type_text(&mut self, text: &str) {
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        if self.rename_tab().is_none() {
            self.refilter();
        }
    }

    /// Delete the last character.
    pub fn backspace(&mut self) {
        self.query.pop();
        if self.rename_tab().is_none() {
            self.refilter();
        }
    }

    fn rename_tab(&self) -> Option<usize> {
        match self.mode {
            Mode::Rename { tab } => Some(tab),
            _ => None,
        }
    }

    /// Move the selection by `delta`, wrapping. Returns true if it moved.
    pub fn move_by(&mut self, delta: isize) -> bool {
        let n = self.matches.len();
        if n < 2 {
            return false;
        }
        let n = isize::try_from(n).unwrap_or(isize::MAX);
        let cur = isize::try_from(self.selected).unwrap_or(0);
        self.selected = usize::try_from((cur + delta).rem_euclid(n)).unwrap_or(0);
        true
    }

    /// Visible entries in order: (entry index, label, selected).
    pub fn rows(&self) -> impl Iterator<Item = (usize, &str, bool)> {
        self.matches
            .iter()
            .enumerate()
            .map(move |(i, e)| (*e, self.entries[*e].as_str(), i == self.selected))
    }

    /// The selected entry's index into the entries list.
    #[must_use]
    pub fn selected_entry(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    /// Position of the selection among the visible rows.
    #[must_use]
    pub fn selected_row(&self) -> usize {
        self.selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn fuzzy_matches_subsequences_ignoring_case() {
        assert!(fuzzy_score("sr", "Split Right").is_some());
        assert!(fuzzy_score("SPLR", "split right").is_some());
        assert!(fuzzy_score("rs", "Split Right").is_none(), "order matters");
        assert!(fuzzy_score("xyz", "Split Right").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn word_starts_and_runs_rank_higher() {
        let items = labels(&["Resize Pane Down", "Split Down", "Decrease Font Size"]);
        assert_eq!(filter("sd", &items)[0], 1, "Split Down: two word starts");
        let items = labels(&["Next Tab", "New Tab", "New Window"]);
        assert_eq!(filter("new w", &items), vec![2], "spaces are ignored, w must follow");
        assert_eq!(filter("newt", &items)[0], 1);
        // "pd": scattered letters at word starts beat a mid-word match.
        let items = labels(&["Split Down", "Resize Pane Down"]);
        assert_eq!(filter("pd", &items)[0], 1);
        let items = labels(&["Paste", "Previous Tab", "Close Pane"]);
        assert_eq!(filter("pa", &items)[0], 0, "consecutive prefix beats scattered");
    }

    #[test]
    fn consecutive_matches_rank_higher() {
        // Same word-start status and the same total gap; only the run differs.
        let items = labels(&["xaxby", "xxaby"]);
        assert_eq!(filter("ab", &items), vec![1, 0]);
    }

    #[test]
    fn previews_end_with_the_theme_picker() {
        assert!(preview_survives(Some(&Mode::Themes { previous: None })));
        assert!(!preview_survives(Some(&Mode::Commands)), "replaced by the command list");
        assert!(!preview_survives(Some(&Mode::Rename { tab: 0 })));
        assert!(!preview_survives(None), "closed");
    }

    #[test]
    fn copy_and_paste_run_in_the_pane() {
        assert!(runs_in_pane(Action::Copy));
        assert!(runs_in_pane(Action::Paste));
        assert!(!runs_in_pane(Action::SplitRight));
        // Every other palette command is handled by the window's `run`.
        for (c, _) in commands() {
            if let Command::Run(a) = c {
                assert_eq!(runs_in_pane(a), matches!(a, Action::Copy | Action::Paste));
            }
        }
    }

    #[test]
    fn empty_query_keeps_order() {
        let items = labels(&["b", "a", "c"]);
        assert_eq!(filter("", &items), vec![0, 1, 2]);
    }

    #[test]
    fn every_built_action_has_a_command_with_a_shortcut_name() {
        let cmds = commands();
        assert_eq!(cmds[0], (Command::SwitchTheme, "Switch Theme"));
        assert_eq!(cmds[1], (Command::CopyShellIntegration, "Copy Shell Integration Line"));
        assert_eq!(cmds[2], (Command::PairedTools, "Paired Tools (Session API)"));
        for a in Action::ALL {
            let listed = cmds.iter().any(|(c, _)| *c == Command::Run(*a));
            let built = crate::keybind::gpui_action(*a).is_some();
            assert_eq!(
                listed,
                built && *a != Action::CommandPalette,
                "{} listed={listed} built={built}",
                a.name()
            );
        }
    }

    #[test]
    fn selection_wraps_and_resets_on_typing() {
        let mut m =
            Model::new(Mode::Commands, labels(&["One", "Two", "Three"]), String::new(), None);
        assert_eq!(m.selected_entry(), Some(0));
        m.move_by(-1);
        assert_eq!(m.selected_entry(), Some(2));
        m.move_by(1);
        assert_eq!(m.selected_entry(), Some(0));
        m.type_text("t");
        assert_eq!(m.rows().count(), 2);
        assert_eq!(m.selected_row(), 0);
        m.backspace();
        assert_eq!(m.rows().count(), 3);
        m.type_text("zz");
        assert_eq!(m.selected_entry(), None);
    }

    #[test]
    fn initial_selection_points_at_the_current_theme() {
        let m = Model::new(
            Mode::Themes { previous: None },
            labels(&["Halyard Night", "Halyard Day", "Kelp"]),
            String::new(),
            Some(2),
        );
        assert_eq!(m.selected_entry(), Some(2));
    }

    #[test]
    fn rename_mode_edits_text_without_filtering() {
        let mut m = Model::new(Mode::Rename { tab: 1 }, Vec::new(), "logs".into(), None);
        m.type_text("!\u{7}");
        assert_eq!(m.query, "logs!");
        m.backspace();
        assert_eq!(m.query, "logs");
        assert_eq!(m.rows().count(), 0);
    }
}

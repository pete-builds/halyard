//! A one-line text field's state, without any drawing: the text, the
//! caret, and whether everything is selected. Used to rename a tab in place.

/// What a key does to the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Delete before the caret (or the selection).
    Backspace,
    /// Delete after the caret (or the selection).
    Delete,
    /// Caret one character left.
    Left,
    /// Caret one character right.
    Right,
    /// Caret to the start.
    Home,
    /// Caret to the end.
    End,
    /// Select everything.
    SelectAll,
}

/// A single-line text field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    /// Caret position, in characters.
    caret: usize,
    /// Everything is selected: typing replaces it, deleting clears it.
    all: bool,
}

impl LineEdit {
    /// A field holding `text` with all of it selected, the way a rename
    /// starts on every platform.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self { text: text.to_owned(), caret: text.chars().count(), all: !text.is_empty() }
    }

    /// The text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether everything is selected.
    #[must_use]
    pub fn all_selected(&self) -> bool {
        self.all
    }

    /// The text before and after the caret.
    #[must_use]
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.byte(self.caret))
    }

    fn byte(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(b, _)| b)
    }

    fn take_selection(&mut self) -> bool {
        if std::mem::take(&mut self.all) {
            self.text.clear();
            self.caret = 0;
            true
        } else {
            false
        }
    }

    /// Insert typed text at the caret (replacing a full selection). Control
    /// characters are dropped.
    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        if s.is_empty() {
            return;
        }
        self.take_selection();
        let at = self.byte(self.caret);
        self.text.insert_str(at, &s);
        self.caret += s.chars().count();
    }

    /// Apply an editing key.
    pub fn apply(&mut self, edit: Edit) {
        let len = self.text.chars().count();
        match edit {
            Edit::Backspace => {
                if !self.take_selection() && self.caret > 0 {
                    self.caret -= 1;
                    let at = self.byte(self.caret);
                    self.text.remove(at);
                }
            }
            Edit::Delete => {
                if !self.take_selection() && self.caret < len {
                    let at = self.byte(self.caret);
                    self.text.remove(at);
                }
            }
            Edit::Left => {
                // Like a text field: Left with all selected goes to the start.
                self.caret =
                    if std::mem::take(&mut self.all) { 0 } else { self.caret.saturating_sub(1) };
            }
            Edit::Right => {
                self.caret =
                    if std::mem::take(&mut self.all) { len } else { (self.caret + 1).min(len) };
            }
            Edit::Home => {
                self.all = false;
                self.caret = 0;
            }
            Edit::End => {
                self.all = false;
                self.caret = len;
            }
            Edit::SelectAll => {
                self.all = len > 0;
                self.caret = len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_everything_selected_so_typing_replaces() {
        let mut e = LineEdit::new("zsh");
        assert!(e.all_selected());
        e.insert("logs");
        assert_eq!(e.text(), "logs");
        assert!(!e.all_selected());
        assert_eq!(e.split(), ("logs", ""));
    }

    #[test]
    fn arrows_move_the_caret_and_keys_edit_there() {
        let mut e = LineEdit::new("abc");
        e.apply(Edit::Left);
        assert_eq!(e.split(), ("", "abc"), "Left with all selected goes to the start");
        e.apply(Edit::Right);
        e.insert("X");
        assert_eq!(e.text(), "aXbc");
        e.apply(Edit::Backspace);
        assert_eq!(e.text(), "abc");
        e.apply(Edit::Delete);
        assert_eq!(e.text(), "ac");
        e.apply(Edit::End);
        e.apply(Edit::Delete);
        assert_eq!(e.text(), "ac", "Delete at the end does nothing");
        e.apply(Edit::Home);
        e.apply(Edit::Backspace);
        assert_eq!(e.text(), "ac", "Backspace at the start does nothing");
    }

    #[test]
    fn deleting_a_full_selection_clears_it() {
        let mut e = LineEdit::new("build");
        e.apply(Edit::Backspace);
        assert_eq!(e.text(), "");
        let mut e = LineEdit::new("build");
        e.apply(Edit::Delete);
        assert_eq!(e.text(), "");
        e.apply(Edit::SelectAll);
        assert!(!e.all_selected(), "nothing to select");
    }

    #[test]
    fn handles_wide_characters_and_drops_control_characters() {
        let mut e = LineEdit::new("");
        e.insert("é🙂\u{7}x\n");
        assert_eq!(e.text(), "é🙂x");
        e.apply(Edit::Left);
        e.apply(Edit::Backspace);
        assert_eq!(e.text(), "éx");
        assert_eq!(e.split(), ("é", "x"));
    }
}

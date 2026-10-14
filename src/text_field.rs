//! Editing logic for a single-line text box (the popup search box).
//! Positions are byte offsets on char boundaries; drawing converts to UTF-16.

const MAX_CHARS: usize = 200;

#[derive(Default, Debug)]
pub struct TextField {
    text: String,
    caret: usize,
    all_selected: bool,
}

impl TextField {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn all_selected(&self) -> bool {
        self.all_selected && !self.text.is_empty()
    }

    /// Caret position in UTF-16 code units (what DirectWrite hit-testing uses).
    pub fn caret_utf16(&self) -> usize {
        self.text[..self.caret].encode_utf16().count()
    }

    pub fn set_caret_utf16(&mut self, pos: usize) {
        let mut units = 0;
        self.caret = self.text.len();
        for (i, c) in self.text.char_indices() {
            if units >= pos {
                self.caret = i;
                break;
            }
            units += c.len_utf16();
        }
        self.all_selected = false;
    }

    /// Inserts typed or pasted text. Line breaks and tabs become spaces,
    /// other control characters are dropped, and the total length is capped.
    pub fn insert(&mut self, s: &str) {
        if self.all_selected {
            self.clear();
        }
        let room = MAX_CHARS.saturating_sub(self.text.chars().count());
        let clean: String = s
            .chars()
            .filter_map(|c| match c {
                '\t' | '\r' | '\n' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .take(room)
            .collect();
        self.text.insert_str(self.caret, &clean);
        self.caret += clean.len();
    }

    pub fn backspace(&mut self) {
        if self.all_selected() {
            return self.clear();
        }
        if let Some(prev) = self.prev_boundary() {
            self.text.replace_range(prev..self.caret, "");
            self.caret = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.all_selected() {
            return self.clear();
        }
        if let Some(next) = self.next_boundary() {
            self.text.replace_range(self.caret..next, "");
        }
    }

    /// Ctrl+Backspace: delete back to the start of the previous word.
    pub fn delete_word_back(&mut self) {
        if self.all_selected() {
            return self.clear();
        }
        let before = &self.text[..self.caret];
        let trimmed = before.trim_end();
        // The word starts just after the last space; a space can be several
        // bytes long (U+00A0, U+3000), so step over its full width.
        let start = trimmed.char_indices().rev().find(|(_, c)| c.is_whitespace()).map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
    }

    pub fn left(&mut self) {
        if self.all_selected() {
            self.all_selected = false;
            self.caret = 0;
        } else if let Some(prev) = self.prev_boundary() {
            self.caret = prev;
        }
    }

    pub fn right(&mut self) {
        if self.all_selected() {
            self.all_selected = false;
            self.caret = self.text.len();
        } else if let Some(next) = self.next_boundary() {
            self.caret = next;
        }
    }

    pub fn home(&mut self) {
        self.all_selected = false;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.all_selected = false;
        self.caret = self.text.len();
    }

    pub fn select_all(&mut self) {
        self.all_selected = !self.text.is_empty();
        self.caret = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.all_selected = false;
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.caret].char_indices().next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.caret..].chars().next().map(|c| self.caret + c.len_utf8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(s: &str) -> TextField {
        let mut f = TextField::default();
        f.insert(s);
        f
    }

    #[test]
    fn typing_appends_at_the_caret() {
        let mut f = field("dance");
        f.home();
        f.insert("happy ");
        assert_eq!(f.text(), "happy dance");
    }

    #[test]
    fn backspace_and_delete_respect_multibyte_characters() {
        let mut f = field("été");
        f.backspace();
        assert_eq!(f.text(), "ét");
        f.home();
        f.delete();
        assert_eq!(f.text(), "t");
    }

    #[test]
    fn select_all_then_typing_replaces() {
        let mut f = field("celeb");
        f.select_all();
        f.insert("gif");
        assert_eq!(f.text(), "gif");
    }

    #[test]
    fn select_all_then_backspace_clears() {
        let mut f = field("zzzz");
        f.select_all();
        f.backspace();
        assert!(f.is_empty());
    }

    #[test]
    fn ctrl_backspace_deletes_a_word() {
        let mut f = field("happy dance  ");
        f.delete_word_back();
        assert_eq!(f.text(), "happy ");
        f.delete_word_back();
        assert_eq!(f.text(), "");
    }

    #[test]
    fn ctrl_backspace_after_a_multibyte_space_keeps_the_space() {
        // U+3000 (typed by Japanese and Chinese input methods), U+00A0
        // (pasted text) and U+2003 used to panic here.
        for space in ['\u{3000}', '\u{00A0}', '\u{2003}'] {
            let mut f = field(&format!("a{space}b"));
            f.delete_word_back();
            assert_eq!(f.text(), format!("a{space}"));
            f.delete_word_back();
            assert_eq!(f.text(), "");
        }
        let mut f = field("猫\u{3000}犬");
        f.delete_word_back();
        assert_eq!(f.text(), "猫\u{3000}");
        f.insert("鳥");
        assert_eq!(f.text(), "猫\u{3000}鳥");
    }

    #[test]
    fn ctrl_backspace_never_panics_on_any_caret_position() {
        let text = "é\u{3000}😀 \u{00A0}x\u{2028}y";
        for units in 0..=text.encode_utf16().count() {
            let mut f = field(text);
            f.set_caret_utf16(units);
            let after = text[f.caret..].to_string();
            f.delete_word_back();
            // Only text before the caret goes; the caret stays on a char boundary.
            assert!(f.text().is_char_boundary(f.caret));
            assert_eq!(&f.text()[f.caret..], after);
            assert!(text.starts_with(&f.text()[..f.caret]));
        }
    }

    #[test]
    fn pasted_line_breaks_become_spaces_and_controls_are_dropped() {
        let f = field("a\r\nb\u{7}c");
        assert_eq!(f.text(), "a  bc");
    }

    #[test]
    fn length_is_capped() {
        let f = field(&"x".repeat(500));
        assert_eq!(f.text().chars().count(), MAX_CHARS);
    }

    #[test]
    fn caret_converts_to_and_from_utf16() {
        let mut f = field("a😀b");
        assert_eq!(f.caret_utf16(), 4);
        f.set_caret_utf16(1);
        f.insert("x");
        assert_eq!(f.text(), "ax😀b");
    }
}

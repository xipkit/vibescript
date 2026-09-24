//! A single-line editor with the key behavior of the Charm bubbles text input
//! that the Go REPL uses: character and word motion and deletion, a character
//! limit, and a horizontally scrolling window.

use unicode_width::UnicodeWidthChar;

/// The most characters one input line holds, as in the Go REPL.
pub const CHAR_LIMIT: usize = 500;

/// Editable text and a cursor between characters.
#[derive(Clone, Debug, Default)]
pub struct Editor {
    value: Vec<char>,
    cursor: usize,
    /// The first character shown when the text is wider than the window.
    offset: usize,
}

/// An editing action, independent of the key that requested it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edit {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Backspace,
    Delete,
    DeleteWordLeft,
    DeleteWordRight,
    DeleteToStart,
}

impl Editor {
    /// Returns the text.
    pub fn value(&self) -> String {
        self.value.iter().collect()
    }

    /// Replaces the text, cleaning control characters and keeping at most
    /// [`CHAR_LIMIT`] characters. The cursor stays where it was when it still
    /// fits, and moves to the end otherwise or when the input was empty.
    pub fn set_value(&mut self, text: &str) {
        let empty = self.value.is_empty();
        self.value = sanitize(text).take(CHAR_LIMIT).collect();
        if (self.cursor == 0 && empty) || self.cursor > self.value.len() {
            self.cursor = self.value.len();
        }
    }

    /// Clears the text.
    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.offset = 0;
    }

    /// Moves the cursor to the end.
    pub fn cursor_end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Inserts typed or pasted text at the cursor, up to the character limit.
    pub fn insert(&mut self, text: &str) {
        let room = CHAR_LIMIT.saturating_sub(self.value.len());
        let inserted: Vec<char> = sanitize(text).take(room).collect();
        let count = inserted.len();
        self.value.splice(self.cursor..self.cursor, inserted);
        self.cursor += count;
    }

    /// Applies an editing action.
    pub fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Left => self.cursor = self.cursor.saturating_sub(1),
            Edit::Right => self.cursor = (self.cursor + 1).min(self.value.len()),
            Edit::WordLeft => self.cursor = self.word_start(),
            Edit::WordRight => self.cursor = self.word_end(),
            Edit::Home => self.cursor = 0,
            Edit::End => self.cursor = self.value.len(),
            Edit::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.value.remove(self.cursor);
                }
            }
            Edit::Delete => {
                if self.cursor < self.value.len() {
                    self.value.remove(self.cursor);
                }
            }
            Edit::DeleteWordLeft => {
                let start = self.delete_word_start();
                self.value.drain(start..self.cursor);
                self.cursor = start;
            }
            Edit::DeleteWordRight => {
                // Like the text input, the character under the cursor goes
                // first, then any spaces, then the rest of that word.
                let mut end = (self.cursor + 1).min(self.value.len());
                while end < self.value.len() && self.value[end].is_whitespace() {
                    end += 1;
                }
                while end < self.value.len() && !self.value[end].is_whitespace() {
                    end += 1;
                }
                self.value.drain(self.cursor..end);
            }
            Edit::DeleteToStart => {
                self.value.drain(..self.cursor);
                self.cursor = 0;
                self.offset = 0;
            }
        }
    }

    /// The start of the word before the cursor, skipping spaces first.
    fn word_start(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.value[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.value[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    /// The end of the word after the cursor, skipping spaces first.
    fn word_end(&self) -> usize {
        let mut i = self.cursor;
        while i < self.value.len() && self.value[i].is_whitespace() {
            i += 1;
        }
        while i < self.value.len() && !self.value[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Where deleting the previous word stops: like [`Self::word_start`], but
    /// the space before the word is kept unless the word starts the line.
    fn delete_word_start(&self) -> usize {
        if self.cursor == 0 {
            return 0;
        }
        let mut i = self.cursor - 1;
        while i > 0 && self.value[i].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.value[i].is_whitespace() {
            i -= 1;
        }
        if i > 0 { i + 1 } else { 0 }
    }

    /// Returns the visible part of the text for a window `width` columns wide
    /// and the cursor's column within it, scrolling only as far as needed to
    /// keep the cursor in view.
    pub fn window(&mut self, width: usize) -> (String, usize) {
        let widths: Vec<usize> = self.value.iter().map(|&ch| char_width(ch)).collect();
        if width == 0 || widths.iter().sum::<usize>() <= width {
            self.offset = 0;
            let column = widths[..self.cursor].iter().sum();
            return (self.value(), column);
        }
        self.offset = self.offset.min(self.cursor);
        while self.offset < self.cursor
            && widths[self.offset..self.cursor].iter().sum::<usize>() >= width
        {
            self.offset += 1;
        }
        let mut end = self.offset;
        let mut used = 0;
        while end < self.value.len() && used + widths[end] <= width {
            used += widths[end];
            end += 1;
        }
        let column = widths[self.offset..self.cursor].iter().sum();
        (self.value[self.offset..end].iter().collect(), column)
    }
}

/// Replaces tabs and line breaks with spaces and drops other control
/// characters, as the text input's sanitizer does.
fn sanitize(text: &str) -> impl Iterator<Item = char> + '_ {
    let mut previous_cr = false;
    text.chars().filter_map(move |ch| {
        let after_cr = std::mem::replace(&mut previous_cr, ch == '\r');
        match ch {
            '\n' if after_cr => None,
            '\t' | '\n' | '\r' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        }
    })
}

/// The terminal columns a character occupies.
pub fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str, cursor: usize) -> Editor {
        let mut editor = Editor::default();
        editor.set_value(text);
        editor.cursor = cursor;
        editor
    }

    #[test]
    fn words_move_and_delete_like_the_text_input() {
        let mut e = editor("foo  bar baz", 12);
        e.edit(Edit::WordLeft);
        assert_eq!(e.cursor, 9);
        e.edit(Edit::WordLeft);
        assert_eq!(e.cursor, 5);
        e.edit(Edit::WordRight);
        assert_eq!(e.cursor, 8);
        let mut e = editor("foo  bar baz", 12);
        e.edit(Edit::DeleteWordLeft);
        assert_eq!((e.value().as_str(), e.cursor), ("foo  bar ", 9));
        e.edit(Edit::DeleteWordLeft);
        assert_eq!((e.value().as_str(), e.cursor), ("foo  ", 5));
        e.edit(Edit::DeleteWordLeft);
        assert_eq!((e.value().as_str(), e.cursor), ("", 0));
        let mut e = editor("foo bar", 3);
        e.edit(Edit::DeleteWordRight);
        assert_eq!((e.value().as_str(), e.cursor), ("foo", 3));
        let mut e = editor("foo bar", 4);
        e.edit(Edit::DeleteToStart);
        assert_eq!((e.value().as_str(), e.cursor), ("bar", 0));
    }

    #[test]
    fn characters_are_inserted_deleted_and_limited() {
        let mut e = Editor::default();
        e.insert("a\tb\r\nc\u{7}");
        assert_eq!(e.value(), "a b c");
        e.edit(Edit::Home);
        e.edit(Edit::Delete);
        e.edit(Edit::End);
        e.edit(Edit::Backspace);
        assert_eq!((e.value().as_str(), e.cursor), (" b ", 3));
        e.insert(&"x".repeat(600));
        assert_eq!(e.value().chars().count(), CHAR_LIMIT);
        e.set_value("short");
        assert_eq!(e.cursor, 5);
    }

    #[test]
    fn the_window_scrolls_to_keep_the_cursor_visible() {
        let mut e = editor("abcdefghij", 10);
        assert_eq!(e.window(20), ("abcdefghij".to_owned(), 10));
        assert_eq!(e.window(10), ("abcdefghij".to_owned(), 10));
        assert_eq!(e.window(5), ("ghij".to_owned(), 4));
        e.edit(Edit::Home);
        assert_eq!(e.window(5), ("abcde".to_owned(), 0));
        e.cursor = 3;
        assert_eq!(e.window(5), ("abcde".to_owned(), 3));
        let mut wide = editor("日本語テキスト", 7);
        assert_eq!(wide.window(6), ("スト".to_owned(), 4));
    }

    #[test]
    fn deleting_the_next_word_takes_the_character_under_the_cursor_first() {
        let mut e = editor("a bc", 0);
        e.edit(Edit::DeleteWordRight);
        assert_eq!(e.value(), "");
        let mut e = editor(" foo", 4);
        e.edit(Edit::DeleteWordLeft);
        assert_eq!(e.value(), "");
    }
}

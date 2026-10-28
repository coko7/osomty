//! Single-line text input with readline-style editing.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Default)]
pub struct LineInput {
    pub text: String,
    /// Byte offset of the cursor, always on a char boundary.
    cursor: usize,
}

impl LineInput {
    pub fn set(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    /// Text before the cursor (used to position the terminal cursor).
    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    /// Apply an editing key. Returns false if the key is not an editing key.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            // Most terminals send Ctrl+Backspace as ^H, which crossterm reports as Ctrl+H.
            KeyCode::Backspace if ctrl || alt => self.delete_to(self.word_start()),
            KeyCode::Char('w' | 'h') if ctrl => self.delete_to(self.word_start()),
            KeyCode::Backspace => self.delete_to(self.prev_char()),
            KeyCode::Delete if ctrl || alt => self.delete_to(self.word_end()),
            KeyCode::Char('d') if alt => self.delete_to(self.word_end()),
            KeyCode::Delete => self.delete_to(self.next_char()),
            KeyCode::Left if ctrl || alt => self.cursor = self.word_start(),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_end(),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(),
            KeyCode::Left => self.cursor = self.prev_char(),
            KeyCode::Right => self.cursor = self.next_char(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char('u') if ctrl => self.delete_to(0),
            KeyCode::Char('k') if ctrl => self.delete_to(self.text.len()),
            KeyCode::Char(c) if !ctrl && !alt => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => return false,
        }
        true
    }

    /// Delete between the cursor and `pos`, leaving the cursor at the start of the range.
    fn delete_to(&mut self, pos: usize) {
        let (from, to) = (pos.min(self.cursor), pos.max(self.cursor));
        self.text.drain(from..to);
        self.cursor = from;
    }

    fn prev_char(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_char(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    /// Start of the word before the cursor, skipping separators first.
    fn word_start(&self) -> usize {
        let mut it = self.text[..self.cursor].char_indices().rev().peekable();
        while it.next_if(|&(_, c)| !is_word(c)).is_some() {}
        while it.next_if(|&(_, c)| is_word(c)).is_some() {}
        it.peek().map_or(0, |&(i, c)| i + c.len_utf8())
    }

    /// End of the word after the cursor, skipping separators first.
    fn word_end(&self) -> usize {
        let after = &self.text[self.cursor..];
        let mut it = after.char_indices().peekable();
        while it.next_if(|&(_, c)| !is_word(c)).is_some() {}
        while it.next_if(|&(_, c)| is_word(c)).is_some() {}
        self.cursor + it.peek().map_or(after.len(), |&(i, _)| i)
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn input(text: &str) -> LineInput {
        let mut i = LineInput::default();
        i.set(text.into());
        i
    }

    #[test]
    fn delete_word_back() {
        let mut i = input("Café de Flore, Paris  ");
        i.handle(key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(i.text, "Café de Flore, ");
        i.handle(key(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(i.text, "Café de ");
        i.handle(key(KeyCode::Backspace, KeyModifiers::CONTROL));
        i.handle(key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(i.text, "");
        assert!(i.handle(key(KeyCode::Char('w'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn word_motion_and_insert() {
        let mut i = input("rue de Rivoli");
        i.handle(key(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(i.before_cursor(), "rue de ");
        i.handle(key(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(i.before_cursor(), "rue ");
        i.handle(key(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(i.before_cursor(), "rue de");
        i.handle(key(KeyCode::Char('s'), KeyModifiers::SHIFT));
        assert_eq!(i.text, "rue des Rivoli");
        i.handle(key(KeyCode::Delete, KeyModifiers::CONTROL));
        assert_eq!(i.text, "rue des");
        i.handle(key(KeyCode::Home, KeyModifiers::NONE));
        i.handle(key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(i.text, "ue des");
    }
}

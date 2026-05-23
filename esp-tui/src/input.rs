use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Single-line text editor with cursor support.
///
/// Tracks an input string and a byte-offset cursor position. All editing
/// methods keep the cursor within valid UTF-8 character boundaries.
pub(crate) struct TextInput {
    input: String,
    cursor: usize,
}

impl TextInput {
    /// Creates an empty input with the cursor at position zero.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
        }
    }

    /// Creates an input pre-filled with `s`, with the cursor at the end.
    ///
    /// # Arguments
    ///
    /// * `s` - The initial content.
    #[must_use]
    pub(crate) fn with_value(s: &str) -> Self {
        Self {
            cursor: s.len(),
            input: s.to_owned(),
        }
    }

    /// Replaces the input text and moves the cursor to the end.
    ///
    /// # Arguments
    ///
    /// * `s` - The new content.
    pub(crate) fn set_value(&mut self, s: &str) {
        s.clone_into(&mut self.input);
        self.cursor = self.input.len();
    }

    /// Returns the current input text.
    #[must_use]
    pub(crate) fn value(&self) -> &str {
        &self.input
    }

    /// Returns the current cursor position as a byte offset into
    /// [`value`](Self::value).
    #[must_use]
    pub(crate) fn cursor_pos(&self) -> usize {
        self.cursor
    }

    /// Inserts `ch` at the cursor position and advances the cursor.
    ///
    /// # Arguments
    ///
    /// * `ch` - The character to insert.
    pub(crate) fn push_char(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Deletes the character immediately before the cursor.
    pub(crate) fn backspace(&mut self) {
        if self.cursor > 0 {
            let before = &self.input[..self.cursor];
            let char_start = before.char_indices().next_back().map_or(0, |(i, _)| i);
            self.input.drain(char_start..self.cursor);
            self.cursor = char_start;
        }
    }

    /// Deletes the character at the cursor position (forward delete).
    pub(crate) fn delete_forward(&mut self) {
        if self.cursor < self.input.len() {
            let ch_len = self.input[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
            self.input.drain(self.cursor..self.cursor + ch_len);
        }
    }

    /// Moves the cursor to the start of the input.
    pub(crate) fn move_cursor_to_start(&mut self) {
        self.cursor = 0;
    }

    /// Moves the cursor to the end of the input.
    pub(crate) fn move_cursor_to_end(&mut self) {
        self.cursor = self.input.len();
    }

    /// Clears all input text and resets the cursor to zero.
    pub(crate) fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    /// Deletes from the cursor to the end of the input.
    pub(crate) fn kill_to_end(&mut self) {
        self.input.truncate(self.cursor);
    }

    /// Deletes from the start of the input to the cursor.
    pub(crate) fn kill_to_start(&mut self) {
        self.input.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Deletes the word immediately before the cursor, stopping at
    /// whitespace or `/` boundaries.
    pub(crate) fn kill_word_back(&mut self) {
        self.kill_word_back_sep(|c| c == '/' || c.is_whitespace());
    }

    /// Deletes the word immediately before the cursor, treating characters
    /// for which `is_sep` returns `true` as word boundaries.
    ///
    /// # Arguments
    ///
    /// * `is_sep` - Predicate that identifies word-separator characters.
    pub(crate) fn kill_word_back_sep(&mut self, is_sep: impl Fn(char) -> bool) {
        if self.cursor != 0 {
            let before = &self.input[..self.cursor];
            let trimmed = before.trim_end_matches(|c: char| is_sep(c));
            let word_start = trimmed.rfind(|c: char| is_sep(c)).map_or(0, |i| i + 1);
            self.input.drain(word_start..self.cursor);
            self.cursor = word_start;
        }
    }

    /// Moves the cursor left or right by `delta` character positions,
    /// clamped to the input bounds.
    ///
    /// # Arguments
    ///
    /// * `delta` - Negative to move left, positive to move right.
    pub(crate) fn move_cursor(&mut self, delta: isize) {
        if delta < 0 {
            let steps = (-delta).cast_unsigned();
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .rev()
                .take(steps)
                .last()
                .map_or(0, |(i, _)| i);
        } else {
            let steps = delta.cast_unsigned();
            self.cursor = self.input[self.cursor..]
                .char_indices()
                .take(steps)
                .last()
                .map_or(self.cursor, |(i, ch)| self.cursor + i + ch.len_utf8());
        }
    }

    /// Handles a text-editing key event. Returns `true` if the key was
    /// consumed, `false` if it is not a recognized text-editing key.
    ///
    /// Supported bindings: printable character inserts at cursor; Backspace
    /// deletes before cursor; Delete/Ctrl+D deletes at cursor; Left/Right
    /// move cursor one character; Home/Ctrl+A move to start; End/Ctrl+E move
    /// to end; Ctrl+K kills to end; Ctrl+U kills to start; Ctrl+W kills word
    /// back (whitespace boundary); Ctrl+L clears.
    ///
    /// # Arguments
    ///
    /// * `key` - The key event to process.
    ///
    /// # Returns
    ///
    /// `true` if the event was consumed.
    pub(crate) fn apply_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('a') => {
                    self.move_cursor_to_start();
                    true
                }
                KeyCode::Char('e') => {
                    self.move_cursor_to_end();
                    true
                }
                KeyCode::Char('l') => {
                    self.clear_input();
                    true
                }
                KeyCode::Char('d') => {
                    self.delete_forward();
                    true
                }
                KeyCode::Char('k') => {
                    self.kill_to_end();
                    true
                }
                KeyCode::Char('u') => {
                    self.kill_to_start();
                    true
                }
                KeyCode::Char('w') => {
                    self.kill_word_back();
                    true
                }
                _ => false,
            }
        } else {
            match key.code {
                KeyCode::Char(ch) => {
                    self.push_char(ch);
                    true
                }
                KeyCode::Backspace => {
                    self.backspace();
                    true
                }
                KeyCode::Delete => {
                    self.delete_forward();
                    true
                }
                KeyCode::Left => {
                    self.move_cursor(-1);
                    true
                }
                KeyCode::Right => {
                    self.move_cursor(1);
                    true
                }
                KeyCode::Home => {
                    self.move_cursor_to_start();
                    true
                }
                KeyCode::End => {
                    self.move_cursor_to_end();
                    true
                }
                _ => false,
            }
        }
    }
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn ti(s: &str) -> TextInput {
        TextInput::with_value(s)
    }

    #[test]
    fn push_char_inserts_and_advances() {
        let mut t = TextInput::new();
        t.push_char('a');
        t.push_char('b');
        assert_eq!(t.value(), "ab");
        assert_eq!(t.cursor_pos(), 2);
    }

    #[test]
    fn push_char_mid_string() {
        let mut t = ti("ac");
        t.move_cursor(-1);
        t.push_char('b');
        assert_eq!(t.value(), "abc");
        assert_eq!(t.cursor_pos(), 2);
    }

    #[test]
    fn backspace_removes_before_cursor() {
        let mut t = ti("abc");
        t.backspace();
        assert_eq!(t.value(), "ab");
        assert_eq!(t.cursor_pos(), 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut t = TextInput::new();
        t.backspace();
        assert_eq!(t.value(), "");
        assert_eq!(t.cursor_pos(), 0);
    }

    #[test]
    fn delete_forward_removes_at_cursor() {
        let mut t = ti("abc");
        t.move_cursor(-2);
        t.delete_forward();
        assert_eq!(t.value(), "ac");
        assert_eq!(t.cursor_pos(), 1);
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut t = ti("abc");
        t.delete_forward();
        assert_eq!(t.value(), "abc");
    }

    #[test]
    fn move_cursor_left_right() {
        let mut t = ti("abc");
        t.move_cursor(-1);
        assert_eq!(t.cursor_pos(), 2);
        t.move_cursor(1);
        assert_eq!(t.cursor_pos(), 3);
    }

    #[test]
    fn move_cursor_clamps() {
        let mut t = ti("ab");
        t.move_cursor(-100);
        assert_eq!(t.cursor_pos(), 0);
        t.move_cursor(100);
        assert_eq!(t.cursor_pos(), 2);
    }

    #[test]
    fn move_to_start_and_end() {
        let mut t = ti("hello");
        t.move_cursor_to_start();
        assert_eq!(t.cursor_pos(), 0);
        t.move_cursor_to_end();
        assert_eq!(t.cursor_pos(), 5);
    }

    #[test]
    fn clear_input_empties_and_resets_cursor() {
        let mut t = ti("hello");
        t.clear_input();
        assert_eq!(t.value(), "");
        assert_eq!(t.cursor_pos(), 0);
    }

    #[test]
    fn kill_to_end_from_mid() {
        let mut t = ti("hello");
        t.move_cursor(-3);
        t.kill_to_end();
        assert_eq!(t.value(), "he");
        assert_eq!(t.cursor_pos(), 2);
    }

    #[test]
    fn kill_to_start_from_mid() {
        let mut t = ti("hello");
        t.move_cursor(-3);
        t.kill_to_start();
        assert_eq!(t.value(), "llo");
        assert_eq!(t.cursor_pos(), 0);
    }

    #[test]
    fn kill_word_back_whitespace_boundary() {
        let mut t = ti("foo bar");
        t.kill_word_back();
        assert_eq!(t.value(), "foo ");
        assert_eq!(t.cursor_pos(), 4);
    }

    #[test]
    fn kill_word_back_sep_slash_boundary() {
        let mut t = ti("/foo/bar");
        t.kill_word_back_sep(|c| c == '/');
        assert_eq!(t.value(), "/foo/");
        assert_eq!(t.cursor_pos(), 5);
    }

    #[test]
    fn with_value_prefills_cursor_at_end() {
        let t = TextInput::with_value("hello");
        assert_eq!(t.value(), "hello");
        assert_eq!(t.cursor_pos(), 5);
    }

    #[test]
    fn set_value_replaces_content() {
        let mut t = ti("old");
        t.set_value("new");
        assert_eq!(t.value(), "new");
        assert_eq!(t.cursor_pos(), 3);
    }

    #[test]
    fn apply_key_char_inserts() {
        let mut t = TextInput::new();
        assert!(t.apply_key(key(KeyCode::Char('x'))));
        assert_eq!(t.value(), "x");
    }

    #[test]
    fn apply_key_backspace_deletes() {
        let mut t = ti("ab");
        t.apply_key(key(KeyCode::Backspace));
        assert_eq!(t.value(), "a");
    }

    #[test]
    fn apply_key_left_right_moves_cursor() {
        let mut t = ti("ab");
        t.apply_key(key(KeyCode::Left));
        assert_eq!(t.cursor_pos(), 1);
        t.apply_key(key(KeyCode::Right));
        assert_eq!(t.cursor_pos(), 2);
    }

    #[test]
    fn apply_key_ctrl_a_moves_to_start() {
        let mut t = ti("hello");
        t.apply_key(ctrl(KeyCode::Char('a')));
        assert_eq!(t.cursor_pos(), 0);
    }

    #[test]
    fn apply_key_ctrl_e_moves_to_end() {
        let mut t = ti("hello");
        t.move_cursor(-3);
        t.apply_key(ctrl(KeyCode::Char('e')));
        assert_eq!(t.cursor_pos(), 5);
    }

    #[test]
    fn apply_key_ctrl_k_kills_to_end() {
        let mut t = ti("hello");
        t.move_cursor(-3);
        t.apply_key(ctrl(KeyCode::Char('k')));
        assert_eq!(t.value(), "he");
    }

    #[test]
    fn apply_key_ctrl_u_kills_to_start() {
        let mut t = ti("hello");
        t.move_cursor(-3);
        t.apply_key(ctrl(KeyCode::Char('u')));
        assert_eq!(t.value(), "llo");
        assert_eq!(t.cursor_pos(), 0);
    }

    #[test]
    fn apply_key_ctrl_w_kills_word() {
        let mut t = ti("foo bar");
        t.apply_key(ctrl(KeyCode::Char('w')));
        assert_eq!(t.value(), "foo ");
    }

    #[test]
    fn apply_key_ctrl_l_clears() {
        let mut t = ti("hello");
        t.apply_key(ctrl(KeyCode::Char('l')));
        assert_eq!(t.value(), "");
    }

    #[test]
    fn apply_key_ctrl_d_deletes_forward() {
        let mut t = ti("abc");
        t.move_cursor(-2);
        t.apply_key(ctrl(KeyCode::Char('d')));
        assert_eq!(t.value(), "ac");
    }

    #[test]
    fn apply_key_home_end() {
        let mut t = ti("abc");
        t.apply_key(key(KeyCode::Home));
        assert_eq!(t.cursor_pos(), 0);
        t.apply_key(key(KeyCode::End));
        assert_eq!(t.cursor_pos(), 3);
    }

    #[test]
    fn apply_key_delete_removes_forward() {
        let mut t = ti("abc");
        t.move_cursor(-2);
        t.apply_key(key(KeyCode::Delete));
        assert_eq!(t.value(), "ac");
    }

    #[test]
    fn apply_key_unknown_returns_false() {
        let mut t = ti("abc");
        assert!(!t.apply_key(key(KeyCode::F(1))));
        assert_eq!(t.value(), "abc");
    }
}

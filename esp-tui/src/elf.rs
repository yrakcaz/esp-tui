use std::io::Read as _;
use std::path::Path;

/// Returns `true` if `path` begins with the ELF magic bytes `\x7fELF`.
///
/// Opens the file, reads exactly four bytes, and compares them against the
/// standard ELF magic header. Returns `false` on any I/O error, if the file
/// is shorter than four bytes, or if the magic does not match.
///
/// # Arguments
///
/// * `path` - Path to the file to inspect.
///
/// # Returns
///
/// `true` when the file starts with `[0x7f, b'E', b'L', b'F']`.
pub(crate) fn is_elf_file(path: &Path) -> bool {
    std::fs::File::open(path)
        .ok()
        .and_then(|mut f| {
            let mut buf = [0u8; 4];
            f.read_exact(&mut buf).ok().map(|()| buf)
        })
        .is_some_and(|buf| buf == [0x7f, b'E', b'L', b'F'])
}

/// State for the ELF path input popup with filesystem tab-completion.
pub(crate) struct Selector {
    input: String,
    cursor: usize,
    completions: Vec<String>,
    completion_cursor: usize,
    /// The parent prefix captured when completions were last computed, so that
    /// cycling always replaces the last segment rather than appending to it.
    completion_parent: String,
}

impl Selector {
    /// Creates a new selector, optionally pre-filled with an existing path.
    ///
    /// # Arguments
    ///
    /// * `prefill` - If `Some`, the input is initialized with this path.
    #[must_use]
    pub(crate) fn new(prefill: Option<&Path>) -> Self {
        let input = prefill.and_then(|p| p.to_str()).unwrap_or("").to_owned();
        let cursor = input.len();
        Self {
            input,
            cursor,
            completions: Vec::new(),
            completion_cursor: 0,
            completion_parent: String::new(),
        }
    }

    fn clear_completions(&mut self) {
        self.completions.clear();
        self.completion_cursor = 0;
        self.completion_parent.clear();
    }

    /// Appends a character at the cursor position and clears completions.
    ///
    /// # Arguments
    ///
    /// * `ch` - The character to insert.
    pub(crate) fn push_char(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.clear_completions();
    }

    /// Removes the character before the cursor and clears completions.
    pub(crate) fn backspace(&mut self) {
        if self.cursor > 0 {
            let before = &self.input[..self.cursor];
            let char_start = before.char_indices().next_back().map_or(0, |(i, _)| i);
            self.input.drain(char_start..self.cursor);
            self.cursor = char_start;
            self.clear_completions();
        }
    }

    /// Moves the text cursor to the beginning of the input and clears
    /// completions.
    pub(crate) fn move_cursor_to_start(&mut self) {
        self.cursor = 0;
        self.clear_completions();
    }

    /// Moves the text cursor to the end of the input and clears completions.
    pub(crate) fn move_cursor_to_end(&mut self) {
        self.cursor = self.input.len();
        self.clear_completions();
    }

    /// Clears the entire input text, resets the cursor, and clears
    /// completions.
    pub(crate) fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.clear_completions();
    }

    /// Deletes the character under the cursor (forward delete) and clears
    /// completions.
    pub(crate) fn delete_forward(&mut self) {
        if self.cursor < self.input.len() {
            let ch = self.input[self.cursor..].chars().next().unwrap_or('\0');
            self.input.drain(self.cursor..self.cursor + ch.len_utf8());
            self.clear_completions();
        }
    }

    /// Deletes from the cursor to the end of the input and clears completions.
    pub(crate) fn kill_to_end(&mut self) {
        self.input.truncate(self.cursor);
        self.clear_completions();
    }

    /// Deletes from the start of the input to the cursor and clears
    /// completions.
    pub(crate) fn kill_to_start(&mut self) {
        self.input.drain(..self.cursor);
        self.cursor = 0;
        self.clear_completions();
    }

    /// Deletes the word immediately before the cursor, stopping at `/`
    /// boundaries, and clears completions.
    pub(crate) fn kill_word_back(&mut self) {
        if self.cursor != 0 {
            let before = &self.input[..self.cursor];
            let trimmed = before.trim_end_matches('/');
            let word_start = trimmed.rfind('/').map_or(0, |i| i + 1);
            self.input.drain(word_start..self.cursor);
            self.cursor = word_start;
            self.clear_completions();
        }
    }

    /// Moves the text cursor left or right, clamped to the input bounds.
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
        self.clear_completions();
    }

    /// Returns the current input string.
    #[must_use]
    pub(crate) fn value(&self) -> &str {
        &self.input
    }

    /// Returns the current text cursor position (byte offset).
    #[must_use]
    pub(crate) fn cursor_pos(&self) -> usize {
        self.cursor
    }

    /// Populates the completion list from the filesystem based on the current
    /// input prefix.
    ///
    /// The parent directory of the current input is listed; entries whose
    /// filename starts with the current filename prefix are included.
    /// Directories are suffixed with `/`. No-op if the parent dir is
    /// unreadable.
    pub(crate) fn complete(&mut self) {
        let (parent_str, prefix) = if self.input.ends_with('/') {
            (self.input.as_str(), "")
        } else if let Some(slash) = self.input.rfind('/') {
            (&self.input[..=slash], &self.input[slash + 1..])
        } else {
            (".", self.input.as_str())
        };

        if let Ok(entries) = std::fs::read_dir(parent_str) {
            let mut completions: Vec<String> = entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name();
                    let name_str = name.to_str()?;
                    let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                    (name_str.starts_with(prefix)
                        && (is_dir || is_elf_file(&entry.path())))
                    .then(|| {
                        if is_dir {
                            format!("{name_str}/")
                        } else {
                            name_str.to_owned()
                        }
                    })
                })
                .collect();
            completions.sort();
            self.completions = completions;
            self.completion_cursor = 0;
            self.completion_parent = if parent_str == "." {
                String::new()
            } else {
                parent_str.to_owned()
            };
        }
    }

    fn apply_highlighted_completion(&mut self) {
        if let Some(completion) = self.completions.get(self.completion_cursor) {
            self.input = format!("{}{completion}", self.completion_parent);
            self.cursor = self.input.len();
        }
    }

    /// Navigates the completion list cursor by `delta`, clamped to bounds,
    /// and writes the newly highlighted completion into the input.
    ///
    /// # Arguments
    ///
    /// * `delta` - Negative to move up, positive to move down.
    pub(crate) fn move_completion(&mut self, delta: isize) {
        if !self.completions.is_empty() {
            self.completion_cursor = self
                .completion_cursor
                .saturating_add_signed(delta)
                .min(self.completions.len() - 1);
            self.apply_highlighted_completion();
        }
    }

    /// Advances the completion cursor by one, wrapping around to the first
    /// entry after the last, and writes the newly highlighted completion into
    /// the input. No-op when the list is empty.
    pub(crate) fn cycle_completion(&mut self) {
        if !self.completions.is_empty() {
            self.completion_cursor =
                (self.completion_cursor + 1) % self.completions.len();
            self.apply_highlighted_completion();
        }
    }

    /// Moves the completion cursor back by one, wrapping to the last entry,
    /// and writes the newly highlighted completion into the input. No-op
    /// when the list is empty.
    pub(crate) fn cycle_completion_back(&mut self) {
        if !self.completions.is_empty() {
            self.completion_cursor = self
                .completion_cursor
                .checked_sub(1)
                .unwrap_or(self.completions.len() - 1);
            self.apply_highlighted_completion();
        }
    }

    /// Dismisses the completion menu, keeping the currently written input.
    ///
    /// If the input has not yet been updated by live cycling, writes the
    /// highlighted entry first. No-op when the list is empty.
    pub(crate) fn accept_completion(&mut self) {
        if !self.completions.is_empty() {
            self.apply_highlighted_completion();
            self.clear_completions();
        }
    }

    /// Performs zsh-style menu tab completion.
    ///
    /// When no completions are loaded, computes them from the filesystem:
    /// - Single result: auto-accepts it; if the result is a directory (ends
    ///   with `/`), immediately computes completions for that directory.
    /// - Multiple results: extends the input to the longest common prefix,
    ///   then writes the first entry into the input and shows the menu.
    ///
    /// When the menu is already showing, cycles to the next entry and writes
    /// it into the input immediately.
    pub(crate) fn tab_complete(&mut self) {
        if self.completions.is_empty() {
            self.complete();
            match self.completions.len() {
                0 => {}
                1 => {
                    self.accept_completion();
                    if self.input.ends_with('/') {
                        self.complete();
                    }
                }
                _ => {
                    self.extend_to_common_prefix();
                    self.completion_cursor = 0;
                    self.apply_highlighted_completion();
                }
            }
        } else {
            self.cycle_completion();
        }
    }

    fn extend_to_common_prefix(&mut self) {
        if let Some(first) = self.completions.first() {
            let prefix = self
                .completions
                .iter()
                .skip(1)
                .fold(first.as_str(), |acc, s| common_prefix(acc, s))
                .to_owned();

            let parent_end = if self.input.ends_with('/') {
                self.input.len()
            } else {
                self.input.rfind('/').map_or(0, |i| i + 1)
            };

            let new_input = format!("{}{prefix}", &self.input[..parent_end]);
            if new_input.len() > self.input.len() {
                self.input = new_input;
                self.cursor = self.input.len();
            }
        }
    }

    /// Returns the current completion list.
    ///
    /// # Returns
    ///
    /// A slice of completion strings; empty when no Tab has been pressed or
    /// after a completion has been accepted.
    #[must_use]
    pub(crate) fn completions(&self) -> &[String] {
        &self.completions
    }

    /// Returns the index of the currently highlighted completion.
    #[must_use]
    pub(crate) fn completion_cursor(&self) -> usize {
        self.completion_cursor
    }
}

fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let end = a
        .char_indices()
        .zip(b.chars())
        .take_while(|((_, ca), cb)| ca == cb)
        .last()
        .map_or(0, |((i, c), _)| i + c.len_utf8());
    &a[..end]
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn sel(input: &str) -> Selector {
        let mut s = Selector::new(None);
        for ch in input.chars() {
            s.push_char(ch);
        }
        s
    }

    #[test]
    fn push_char_appends_and_advances_cursor() {
        let mut s = Selector::new(None);
        s.push_char('a');
        s.push_char('b');
        assert_eq!(s.value(), "ab");
        assert_eq!(s.cursor_pos(), 2);
    }

    #[test]
    fn push_char_clears_completions() {
        let mut s = sel("foo");
        s.completions = vec!["foobar".into()];
        s.push_char('x');
        assert!(s.completions().is_empty());
    }

    #[test]
    fn backspace_removes_char_before_cursor() {
        let mut s = sel("abc");
        s.backspace();
        assert_eq!(s.value(), "ab");
        assert_eq!(s.cursor_pos(), 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut s = Selector::new(None);
        s.backspace();
        assert_eq!(s.value(), "");
        assert_eq!(s.cursor_pos(), 0);
    }

    #[test]
    fn backspace_clears_completions() {
        let mut s = sel("foo");
        s.completions = vec!["foobar".into()];
        s.backspace();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn move_cursor_left_and_right() {
        let mut s = sel("abc");
        s.move_cursor(-1);
        assert_eq!(s.cursor_pos(), 2);
        s.move_cursor(1);
        assert_eq!(s.cursor_pos(), 3);
    }

    #[test]
    fn move_cursor_clamps_to_bounds() {
        let mut s = sel("ab");
        s.move_cursor(-100);
        assert_eq!(s.cursor_pos(), 0);
        s.move_cursor(100);
        assert_eq!(s.cursor_pos(), 2);
    }

    #[test]
    fn value_returns_current_input() {
        let s = sel("hello");
        assert_eq!(s.value(), "hello");
    }

    #[test]
    fn new_with_prefill_sets_input_and_cursor() {
        let s = Selector::new(Some(Path::new("/tmp/app.elf")));
        assert_eq!(s.value(), "/tmp/app.elf");
        assert_eq!(s.cursor_pos(), 12);
    }

    #[test]
    fn complete_populates_matching_entries() {
        let dir = tempdir();
        fs::write(dir.join("app.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("app.elf.map"), b"").unwrap();
        fs::write(dir.join("other"), b"").unwrap();

        let prefix = format!("{}/app", dir.display());
        let mut s = sel(&prefix);
        s.complete();
        assert!(s.completions().contains(&"app.elf".to_owned()));
        assert!(!s.completions().contains(&"app.elf.map".to_owned()));
        assert!(!s.completions().contains(&"other".to_owned()));
    }

    #[test]
    fn complete_suffixes_directories_with_slash() {
        let dir = tempdir();
        fs::create_dir(dir.join("subdir")).unwrap();

        let prefix = format!("{}/sub", dir.display());
        let mut s = sel(&prefix);
        s.complete();
        assert!(s.completions().contains(&"subdir/".to_owned()));
    }

    #[test]
    fn complete_nonexistent_path_leaves_completions_empty() {
        let mut s = sel("/nonexistent/path/prefix");
        s.complete();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn complete_relative_prefix_uses_current_dir() {
        let dir = tempdir();
        fs::write(dir.join("firmware.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        let prefix = format!("{}/firmware", dir.display());
        let mut s = sel(&prefix);
        s.complete();
        assert!(
            s.completions().contains(&"firmware.elf".to_owned()),
            "absolute prefix should complete against that directory"
        );
    }

    #[test]
    fn complete_dot_prefix_shows_hidden_entries() {
        let dir = tempdir();
        fs::create_dir(dir.join(".hidden")).unwrap();
        fs::write(dir.join(".dotfile"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("visible"), b"").unwrap();
        let prefix = format!("{}/.", dir.display());
        let mut s = sel(&prefix);
        s.complete();
        let comps = s.completions().to_vec();
        assert!(
            comps.contains(&".hidden/".to_owned()),
            "should find .hidden dir"
        );
        assert!(
            comps.contains(&".dotfile".to_owned()),
            "should find .dotfile with ELF magic"
        );
        assert!(
            !comps.contains(&"visible".to_owned()),
            "should not show non-ELF files"
        );
    }

    #[test]
    fn accept_completion_dot_prefix() {
        let dir = tempdir();
        fs::create_dir(dir.join(".config")).unwrap();
        let prefix = format!("{}/.", dir.display());
        let mut s = sel(&prefix);
        s.complete();
        s.accept_completion();
        assert!(
            s.value().ends_with("/.config/"),
            "accepted dot-prefix completion should end with /.config/"
        );
    }

    #[test]
    fn move_completion_clamps_to_bounds() {
        let mut s = Selector::new(None);
        s.completions = vec!["a".into(), "b".into(), "c".into()];
        s.move_completion(10);
        assert_eq!(s.completion_cursor(), 2);
        s.move_completion(-10);
        assert_eq!(s.completion_cursor(), 0);
    }

    #[test]
    fn move_completion_empty_list_is_noop() {
        let mut s = Selector::new(None);
        s.move_completion(1);
        assert_eq!(s.completion_cursor(), 0);
    }

    #[test]
    fn accept_completion_replaces_filename_prefix() {
        let dir = tempdir();
        fs::write(dir.join("app.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let prefix = format!("{}/app", dir.display());
        let mut s = sel(&prefix);
        s.complete();
        s.accept_completion();
        assert_eq!(s.value(), format!("{}/app.elf", dir.display()));
        assert!(s.completions().is_empty());
    }

    #[test]
    fn accept_completion_appends_slash_for_dir() {
        let dir = tempdir();
        fs::create_dir(dir.join("mydir")).unwrap();

        let prefix = format!("{}/myd", dir.display());
        let mut s = sel(&prefix);
        s.complete();
        s.accept_completion();
        assert_eq!(s.value(), format!("{}/mydir/", dir.display()));
    }

    #[test]
    fn accept_completion_noop_when_empty() {
        let mut s = sel("foo");
        s.accept_completion();
        assert_eq!(s.value(), "foo");
    }

    #[test]
    fn is_elf_file_returns_true_for_valid_elf() {
        let dir = tempdir();
        let path = dir.join("valid.elf");
        fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        assert!(is_elf_file(&path));
    }

    #[test]
    fn is_elf_file_returns_false_for_non_elf() {
        let dir = tempdir();
        let path = dir.join("not.elf");
        fs::write(&path, b"not an elf").unwrap();
        assert!(!is_elf_file(&path));
    }

    #[test]
    fn is_elf_file_returns_false_for_too_short() {
        let dir = tempdir();
        let path = dir.join("short.elf");
        fs::write(&path, b"\x7fEL").unwrap();
        assert!(!is_elf_file(&path));
    }

    #[test]
    fn is_elf_file_returns_false_for_nonexistent() {
        assert!(!is_elf_file(Path::new("/nonexistent/path.elf")));
    }

    #[test]
    fn move_cursor_to_start_sets_cursor_zero() {
        let mut s = sel("hello");
        s.move_cursor_to_start();
        assert_eq!(s.cursor_pos(), 0);
    }

    #[test]
    fn move_cursor_to_start_clears_completions() {
        let mut s = sel("foo");
        s.completions = vec!["foobar".into()];
        s.move_cursor_to_start();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn move_cursor_to_end_sets_cursor_to_len() {
        let mut s = sel("hello");
        s.move_cursor(-5);
        s.move_cursor_to_end();
        assert_eq!(s.cursor_pos(), 5);
    }

    #[test]
    fn move_cursor_to_end_clears_completions() {
        let mut s = sel("foo");
        s.completions = vec!["foobar".into()];
        s.move_cursor_to_end();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn clear_input_empties_input_and_cursor() {
        let mut s = sel("hello");
        s.completions = vec!["helloworld".into()];
        s.clear_input();
        assert_eq!(s.value(), "");
        assert_eq!(s.cursor_pos(), 0);
        assert!(s.completions().is_empty());
    }

    #[test]
    fn tab_complete_single_match_auto_accepts() {
        let dir = tempdir();
        fs::write(dir.join("app.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let prefix = format!("{}/app", dir.display());
        let mut s = sel(&prefix);
        s.tab_complete();
        assert_eq!(s.value(), format!("{}/app.elf", dir.display()));
        assert!(s.completions().is_empty());
    }

    #[test]
    fn tab_complete_single_dir_match_descends() {
        let dir = tempdir();
        fs::create_dir(dir.join("build")).unwrap();
        fs::write(
            dir.join("build").join("app.elf"),
            b"\x7fELF\x00\x00\x00\x00",
        )
        .unwrap();

        let prefix = format!("{}/bui", dir.display());
        let mut s = sel(&prefix);
        s.tab_complete();
        assert_eq!(s.value(), format!("{}/build/", dir.display()));
        assert!(
            !s.completions().is_empty(),
            "should immediately list build/ contents"
        );
    }

    #[test]
    fn tab_complete_multiple_matches_extend_to_common_prefix() {
        let dir = tempdir();
        fs::write(dir.join("app_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("app_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let prefix = format!("{}/a", dir.display());
        let mut s = sel(&prefix);
        s.tab_complete();
        assert_eq!(s.value(), format!("{}/app_a.elf", dir.display()));
        assert_eq!(s.completions().len(), 2);
        assert_eq!(s.completion_cursor(), 0);
    }

    #[test]
    fn tab_complete_already_showing_cycles() {
        let dir = tempdir();
        fs::write(dir.join("app_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("app_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let prefix = format!("{}/app", dir.display());
        let mut s = sel(&prefix);
        s.tab_complete();
        assert_eq!(s.value(), format!("{}/app_a.elf", dir.display()));
        s.tab_complete();
        assert_eq!(s.completion_cursor(), 1);
        assert_eq!(s.value(), format!("{}/app_b.elf", dir.display()));
    }

    #[test]
    fn tab_complete_no_matches_is_noop() {
        let mut s = sel("/nonexistent/path/prefix");
        s.tab_complete();
        assert_eq!(s.value(), "/nonexistent/path/prefix");
        assert!(s.completions().is_empty());
    }

    #[test]
    fn delete_forward_removes_char_under_cursor() {
        let mut s = sel("abc");
        s.move_cursor(-2);
        s.delete_forward();
        assert_eq!(s.value(), "ac");
        assert_eq!(s.cursor_pos(), 1);
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut s = sel("abc");
        s.delete_forward();
        assert_eq!(s.value(), "abc");
    }

    #[test]
    fn delete_forward_clears_completions() {
        let mut s = sel("foo");
        s.completions = vec!["foobar".into()];
        s.move_cursor(-1);
        s.delete_forward();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn kill_to_end_removes_from_cursor_to_end() {
        let mut s = sel("hello");
        s.move_cursor(-3);
        s.kill_to_end();
        assert_eq!(s.value(), "he");
        assert_eq!(s.cursor_pos(), 2);
    }

    #[test]
    fn kill_to_end_at_end_is_noop() {
        let mut s = sel("hello");
        s.kill_to_end();
        assert_eq!(s.value(), "hello");
    }

    #[test]
    fn kill_to_end_clears_completions() {
        let mut s = sel("foo");
        s.completions = vec!["foobar".into()];
        s.move_cursor(-1);
        s.kill_to_end();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn kill_to_start_removes_from_start_to_cursor() {
        let mut s = sel("hello");
        s.move_cursor(-3);
        s.kill_to_start();
        assert_eq!(s.value(), "llo");
        assert_eq!(s.cursor_pos(), 0);
    }

    #[test]
    fn kill_to_start_at_start_is_noop() {
        let mut s = sel("hello");
        s.move_cursor(-5);
        s.kill_to_start();
        assert_eq!(s.value(), "hello");
    }

    #[test]
    fn kill_to_start_clears_completions() {
        let mut s = sel("foo");
        s.completions = vec!["foobar".into()];
        s.move_cursor(-1);
        s.kill_to_start();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn kill_word_back_removes_last_path_segment() {
        let mut s = sel("/tmp/foo/bar");
        s.kill_word_back();
        assert_eq!(s.value(), "/tmp/foo/");
        assert_eq!(s.cursor_pos(), 9);
    }

    #[test]
    fn kill_word_back_strips_trailing_slashes_first() {
        let mut s = sel("/tmp/foo/");
        s.kill_word_back();
        assert_eq!(s.value(), "/tmp/");
        assert_eq!(s.cursor_pos(), 5);
    }

    #[test]
    fn kill_word_back_at_root_clears_all() {
        let mut s = sel("filename.elf");
        s.kill_word_back();
        assert_eq!(s.value(), "");
        assert_eq!(s.cursor_pos(), 0);
    }

    #[test]
    fn kill_word_back_at_start_is_noop() {
        let mut s = Selector::new(None);
        s.kill_word_back();
        assert_eq!(s.value(), "");
    }

    #[test]
    fn kill_word_back_clears_completions() {
        let mut s = sel("/tmp/foo");
        s.completions = vec!["foobar".into()];
        s.kill_word_back();
        assert!(s.completions().is_empty());
    }

    #[test]
    fn tab_complete_cycling_writes_input_live() {
        let dir = tempdir();
        fs::write(dir.join("app_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("app_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let prefix = format!("{}/a", dir.display());
        let mut s = sel(&prefix);
        s.tab_complete();
        assert_eq!(s.value(), format!("{}/app_a.elf", dir.display()));
        s.tab_complete();
        assert_eq!(s.value(), format!("{}/app_b.elf", dir.display()));
        s.tab_complete();
        assert_eq!(s.value(), format!("{}/app_a.elf", dir.display()));
    }

    #[test]
    fn cycle_completion_back_wraps_and_writes_input() {
        let dir = tempdir();
        fs::write(dir.join("app_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("app_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let prefix = format!("{}/a", dir.display());
        let mut s = sel(&prefix);
        s.tab_complete();
        s.cycle_completion_back();
        assert_eq!(s.completion_cursor(), 1);
        assert_eq!(s.value(), format!("{}/app_b.elf", dir.display()));
        s.cycle_completion_back();
        assert_eq!(s.completion_cursor(), 0);
        assert_eq!(s.value(), format!("{}/app_a.elf", dir.display()));
    }

    #[test]
    fn cycle_completion_back_noop_when_empty() {
        let mut s = Selector::new(None);
        s.cycle_completion_back();
        assert_eq!(s.completion_cursor(), 0);
        assert_eq!(s.value(), "");
    }

    #[test]
    fn move_completion_writes_input_live() {
        let dir = tempdir();
        fs::write(dir.join("app_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("app_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let prefix = format!("{}/a", dir.display());
        let mut s = sel(&prefix);
        s.tab_complete();
        s.move_completion(1);
        assert_eq!(s.value(), format!("{}/app_b.elf", dir.display()));
        s.move_completion(-1);
        assert_eq!(s.value(), format!("{}/app_a.elf", dir.display()));
    }

    #[test]
    fn tab_complete_cycling_dirs_replaces_not_appends() {
        let dir = tempdir();
        fs::create_dir(dir.join("alpha")).unwrap();
        fs::create_dir(dir.join("beta")).unwrap();

        let prefix = dir.display().to_string() + "/";
        let mut s = sel(&prefix);
        s.tab_complete();
        let first = s.value().to_owned();
        assert!(
            first == format!("{}/alpha/", dir.display())
                || first == format!("{}/beta/", dir.display())
        );
        s.tab_complete();
        let second = s.value().to_owned();
        assert_ne!(first, second);
        assert!(
            second == format!("{}/alpha/", dir.display())
                || second == format!("{}/beta/", dir.display())
        );
        s.tab_complete();
        assert_eq!(s.value(), first);
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "esp-tui-elf-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

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
        }
    }

    /// Appends a character at the cursor position and clears completions.
    ///
    /// # Arguments
    ///
    /// * `ch` - The character to insert.
    pub(crate) fn push_char(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.completions.clear();
        self.completion_cursor = 0;
    }

    /// Removes the character before the cursor and clears completions.
    pub(crate) fn backspace(&mut self) {
        if self.cursor > 0 {
            let before = &self.input[..self.cursor];
            let char_start = before.char_indices().next_back().map_or(0, |(i, _)| i);
            self.input.drain(char_start..self.cursor);
            self.cursor = char_start;
            self.completions.clear();
            self.completion_cursor = 0;
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
            for _ in 0..steps {
                if self.cursor == 0 {
                    break;
                }
                let before = &self.input[..self.cursor];
                if let Some((i, _)) = before.char_indices().next_back() {
                    self.cursor = i;
                }
            }
        } else {
            let steps = delta.cast_unsigned();
            for _ in 0..steps {
                if self.cursor >= self.input.len() {
                    break;
                }
                let ch = self.input[self.cursor..].chars().next().unwrap_or('\0');
                self.cursor += ch.len_utf8();
            }
        }
        self.completions.clear();
        self.completion_cursor = 0;
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

        let Ok(entries) = std::fs::read_dir(parent_str) else {
            return;
        };

        let mut completions: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let name_str = name.to_str()?;
                if !name_str.starts_with(prefix) {
                    return None;
                }
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                if !is_dir && !is_elf_file(&entry.path()) {
                    return None;
                }
                let display = if is_dir {
                    format!("{name_str}/")
                } else {
                    name_str.to_owned()
                };
                Some(display)
            })
            .collect();
        completions.sort();
        self.completions = completions;
        self.completion_cursor = 0;
    }

    /// Navigates the completion list cursor by `delta`, clamped to bounds.
    ///
    /// # Arguments
    ///
    /// * `delta` - Negative to move up, positive to move down.
    pub(crate) fn move_completion(&mut self, delta: isize) {
        if self.completions.is_empty() {
            return;
        }
        self.completion_cursor = self
            .completion_cursor
            .saturating_add_signed(delta)
            .min(self.completions.len() - 1);
    }

    /// Advances the completion cursor by one, wrapping around to the first
    /// entry after the last. No-op when the list is empty.
    pub(crate) fn cycle_completion(&mut self) {
        if self.completions.is_empty() {
            return;
        }
        self.completion_cursor =
            (self.completion_cursor + 1) % self.completions.len();
    }

    /// Accepts the currently highlighted completion, replacing the filename
    /// portion of the input.
    pub(crate) fn accept_completion(&mut self) {
        let Some(completion) = self.completions.get(self.completion_cursor) else {
            return;
        };

        let parent = if self.input.ends_with('/') {
            self.input.as_str()
        } else if let Some(slash) = self.input.rfind('/') {
            &self.input[..=slash]
        } else {
            ""
        };

        self.input = format!("{parent}{completion}");
        self.cursor = self.input.len();
        self.completions.clear();
        self.completion_cursor = 0;
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
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let mut s = sel("firmware");
        s.complete();
        let result = s.completions().contains(&"firmware.elf".to_owned());
        std::env::set_current_dir(orig).unwrap();
        assert!(
            result,
            "relative prefix should complete against current directory"
        );
    }

    #[test]
    fn complete_dot_prefix_shows_hidden_entries() {
        let dir = tempdir();
        fs::create_dir(dir.join(".hidden")).unwrap();
        fs::write(dir.join(".dotfile"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        fs::write(dir.join("visible"), b"").unwrap();
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let mut s = sel(".");
        s.complete();
        let comps = s.completions().to_vec();
        std::env::set_current_dir(orig).unwrap();
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
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let mut s = sel(".");
        s.complete();
        s.accept_completion();
        let result = s.value().to_owned();
        std::env::set_current_dir(orig).unwrap();
        assert_eq!(result, ".config/");
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

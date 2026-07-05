use std::collections::HashSet;
use std::hash::Hash;

use crossterm::event::KeyEvent;
use esp_agent_msg as agent_msg;
use regex::Regex;

use crate::input::TextInput;
use crate::log;

fn toggle_in_set<T: Eq + Hash>(set: &mut HashSet<T>, value: T) {
    if !set.remove(&value) {
        set.insert(value);
    }
}

const LEVELS: [log::Level; 5] = [
    log::Level::Error,
    log::Level::Warn,
    log::Level::Info,
    log::Level::Debug,
    log::Level::Verbose,
];

struct FilterSnapshot {
    hidden_levels: HashSet<log::Level>,
    hidden_tags: HashSet<String>,
    search: String,
}

/// Tracks which severity levels and ESP-IDF tags are visible, manages
/// the filter popup state, and holds the compiled regex for inline search.
pub(crate) struct State {
    known_tags: Vec<String>,
    hidden_tags: HashSet<String>,
    hidden_levels: HashSet<log::Level>,
    popup_open: bool,
    cursor: usize,
    search: TextInput,
    search_focused: bool,
    compiled_regex: Option<Regex>,
    regex_error: bool,
    snapshot: Option<FilterSnapshot>,
}

impl State {
    /// Creates a new empty filter state with all levels visible, no tags, and
    /// the popup closed.
    ///
    /// # Returns
    ///
    /// A [`State`] with no hidden levels, no known tags, and the popup closed.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            known_tags: Vec::new(),
            hidden_tags: HashSet::new(),
            hidden_levels: HashSet::new(),
            popup_open: false,
            cursor: 0,
            search: TextInput::new(),
            search_focused: false,
            compiled_regex: None,
            regex_error: false,
            snapshot: None,
        }
    }

    /// Records a tag seen in the stream, adding it to the known list if new.
    ///
    /// # Arguments
    ///
    /// * `tag` - The ESP-IDF tag string to record.
    pub(crate) fn record_tag(&mut self, tag: &str) {
        if !tag.is_empty() && !self.known_tags.iter().any(|t| t == tag) {
            self.known_tags.push(tag.to_owned());
            if tag == agent_msg::TAG {
                self.hidden_tags.insert(tag.to_owned());
            }
        }
    }

    /// Returns whether a log entry should be shown given the current filter.
    ///
    /// # Arguments
    ///
    /// * `entry` - The log entry to test for visibility.
    ///
    /// # Returns
    ///
    /// `true` if neither the entry's level nor its tag is hidden.
    #[must_use]
    pub(crate) fn is_visible(&self, entry: &log::Entry) -> bool {
        !self.hidden_levels.contains(&entry.level())
            && !self.hidden_tags.contains(entry.tag())
    }

    /// Toggles the item at the current cursor position. Cursor indices 0–4
    /// address severity levels; indices 5 and above address the filtered tag
    /// list (tags matching the current search query).
    pub(crate) fn toggle_at_cursor(&mut self) {
        if self.cursor < LEVELS.len() {
            toggle_in_set(&mut self.hidden_levels, LEVELS[self.cursor]);
        } else {
            let tag_idx = self.cursor - LEVELS.len();
            let tag = self.known_tags.get(tag_idx).cloned();
            if let Some(tag) = tag {
                toggle_in_set(&mut self.hidden_tags, tag);
            }
        }
    }

    /// Moves the cursor by `delta` positions, clamped to the total item count.
    /// The tag section is sized by the current filtered tag list.
    ///
    /// # Arguments
    ///
    /// * `delta` - Positive to move down, negative to move up.
    pub(crate) fn move_cursor(&mut self, delta: isize) {
        let total = LEVELS.len() + self.known_tags.len();
        if total > 0 {
            let off = delta.unsigned_abs() % total;
            self.cursor = if delta >= 0 {
                (self.cursor + off) % total
            } else {
                (self.cursor + total - off) % total
            };
        }
    }

    /// Toggles all items: hides everything if all are currently visible,
    /// otherwise makes everything visible.
    pub(crate) fn toggle_all(&mut self) {
        if self.hidden_levels.is_empty() && self.hidden_tags.is_empty() {
            self.hidden_levels.extend(LEVELS.iter().copied());
            self.hidden_tags.extend(self.known_tags.iter().cloned());
        } else {
            self.hidden_levels.clear();
            self.hidden_tags.clear();
        }
    }

    /// Opens the filter popup and snapshots the current state so it can be
    /// restored if the user cancels. The search bar is focused automatically.
    pub(crate) fn open_popup(&mut self) {
        self.snapshot = Some(FilterSnapshot {
            hidden_levels: self.hidden_levels.clone(),
            hidden_tags: self.hidden_tags.clone(),
            search: self.search.value().to_owned(),
        });
        self.popup_open = true;
        self.search_focused = true;
    }

    /// Closes the popup and commits all changes made since opening.
    pub(crate) fn confirm_popup(&mut self) {
        self.snapshot = None;
        self.popup_open = false;
        self.search_focused = false;
    }

    /// Closes the popup and reverts all changes made since opening.
    pub(crate) fn cancel_popup(&mut self) {
        if let Some(snap) = self.snapshot.take() {
            self.hidden_levels = snap.hidden_levels;
            self.hidden_tags = snap.hidden_tags;
            self.search.set_value(&snap.search);
            self.recompile_regex();
        }
        self.popup_open = false;
        self.search_focused = false;
    }

    /// Clears the search query and resets regex state.
    pub(crate) fn clear_search(&mut self) {
        self.search.clear_input();
        self.compiled_regex = None;
        self.regex_error = false;
    }

    /// Returns whether the filter popup is currently open.
    ///
    /// # Returns
    ///
    /// `true` if the popup is visible, `false` if it is hidden.
    #[must_use]
    pub(crate) fn is_popup_open(&self) -> bool {
        self.popup_open
    }

    /// Focuses the search bar, routing all subsequent key input to it.
    pub(crate) fn focus_search(&mut self) {
        self.search_focused = true;
    }

    /// Unfocuses the search bar, returning to navigation mode.
    pub(crate) fn unfocus_search(&mut self) {
        self.search_focused = false;
    }

    /// Returns whether the search bar currently has focus.
    ///
    /// # Returns
    ///
    /// `true` when key input is routed to the search bar.
    #[must_use]
    pub(crate) fn is_search_focused(&self) -> bool {
        self.search_focused
    }

    /// Returns all tags seen so far, in insertion order. This list is never
    /// filtered by the active search query.
    ///
    /// # Returns
    ///
    /// A slice of tag strings in the order they were first recorded.
    #[must_use]
    pub(crate) fn known_tags(&self) -> &[String] {
        &self.known_tags
    }

    /// Returns the fixed ordered list of all severity levels.
    ///
    /// # Returns
    ///
    /// A static slice of all [`log::Level`] variants from most to least severe.
    #[must_use]
    pub(crate) fn levels() -> &'static [log::Level] {
        &LEVELS
    }

    /// Returns whether the given tag is currently hidden.
    ///
    /// # Arguments
    ///
    /// * `tag` - The tag name to check.
    ///
    /// # Returns
    ///
    /// `true` if entries with this tag are filtered out.
    #[must_use]
    pub(crate) fn is_tag_hidden(&self, tag: &str) -> bool {
        self.hidden_tags.contains(tag)
    }

    /// Returns whether the given severity level is currently hidden.
    ///
    /// # Arguments
    ///
    /// * `level` - The level to check.
    ///
    /// # Returns
    ///
    /// `true` if entries at this level are filtered out.
    #[must_use]
    pub(crate) fn is_level_hidden(&self, level: log::Level) -> bool {
        self.hidden_levels.contains(&level)
    }

    /// Returns the current cursor index within the combined level + tag list.
    ///
    /// # Returns
    ///
    /// Zero-based index; values below [`LEVELS`] length address severity levels,
    /// values at or above address filtered tags.
    #[must_use]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the current search query string.
    ///
    /// # Returns
    ///
    /// An empty string when no search is active.
    #[must_use]
    pub(crate) fn search_query(&self) -> &str {
        self.search.value()
    }

    /// Returns the cursor position within the search query as a byte offset.
    #[must_use]
    pub(crate) fn search_cursor(&self) -> usize {
        self.search.cursor_pos()
    }

    /// Applies a text-editing key event to the search input and recompiles the
    /// regex when the query content changes.
    ///
    /// # Arguments
    ///
    /// * `key` - The key event to process.
    ///
    /// # Returns
    ///
    /// `true` when the query string content changed (character inserted,
    /// deleted, or cleared); `false` for cursor-only movements.
    pub(crate) fn apply_search_key(&mut self, key: KeyEvent) -> bool {
        let len_before = self.search.value().len();
        self.search.apply_key(key);
        let changed = self.search.value().len() != len_before;
        if changed {
            self.recompile_regex();
        }
        changed
    }

    fn recompile_regex(&mut self) {
        let query = self.search.value();
        if query.is_empty() {
            self.compiled_regex = None;
            self.regex_error = false;
        } else if let Ok(re) = regex::RegexBuilder::new(query)
            .case_insensitive(true)
            .build()
        {
            self.compiled_regex = Some(re);
            self.regex_error = false;
        } else {
            self.compiled_regex = None;
            self.regex_error = true;
        }
    }

    /// Returns the compiled case-insensitive regex for the current search query,
    /// or `None` when the query is empty or contains an invalid pattern.
    ///
    /// # Returns
    ///
    /// `Some` when the query is a valid non-empty regex; `None` otherwise.
    #[must_use]
    pub(crate) fn compiled_regex(&self) -> Option<&Regex> {
        self.compiled_regex.as_ref()
    }

    /// Returns whether the current search query is a non-empty but invalid regex.
    ///
    /// # Returns
    ///
    /// `true` when the user has typed a pattern that fails to compile.
    #[must_use]
    pub(crate) fn is_regex_error(&self) -> bool {
        self.regex_error
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl State {
    pub(crate) fn push_search_char(&mut self, c: char) {
        self.search.push_char(c);
        self.recompile_regex();
    }

    pub(crate) fn pop_search_char(&mut self) {
        self.search.backspace();
        self.recompile_regex();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log;

    fn tag_cursor(tag_index: usize) -> usize {
        LEVELS.len() + tag_index
    }

    #[test]
    fn records_tags_once() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.record_tag("wifi");
        s.record_tag("i2c");
        assert_eq!(s.known_tags(), &["wifi", "i2c"]);
    }

    #[test]
    fn empty_tag_not_recorded() {
        let mut s = State::new();
        s.record_tag("");
        assert!(s.known_tags().is_empty());
    }

    #[test]
    fn esp_agent_tag_hidden_by_default() {
        let mut s = State::new();
        s.record_tag("esp_agent");
        assert!(s.known_tags().contains(&"esp_agent".to_owned()));
        assert!(s.is_tag_hidden("esp_agent"));
    }

    #[test]
    fn esp_agent_tag_can_be_toggled_visible() {
        let mut s = State::new();
        s.record_tag("esp_agent");
        s.move_cursor(LEVELS.len().cast_signed());
        s.toggle_at_cursor();
        assert!(!s.is_tag_hidden("esp_agent"));
    }

    #[test]
    fn toggle_hides_and_shows_tag() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.move_cursor(tag_cursor(0).cast_signed());
        assert!(!s.is_tag_hidden("wifi"));
        s.toggle_at_cursor();
        assert!(s.is_tag_hidden("wifi"));
        s.toggle_at_cursor();
        assert!(!s.is_tag_hidden("wifi"));
    }

    #[test]
    fn toggle_hides_and_shows_level() {
        let mut s = State::new();
        assert!(!s.is_level_hidden(log::Level::Error));
        s.toggle_at_cursor();
        assert!(s.is_level_hidden(log::Level::Error));
        s.toggle_at_cursor();
        assert!(!s.is_level_hidden(log::Level::Error));
    }

    #[test]
    fn is_visible_respects_hidden_tag() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.move_cursor(tag_cursor(0).cast_signed());
        s.toggle_at_cursor();
        let entry = log::parse_line("I (1) wifi: msg");
        assert!(!s.is_visible(&entry));
        let other = log::parse_line("I (1) i2c: msg");
        assert!(s.is_visible(&other));
    }

    #[test]
    fn is_visible_respects_hidden_level() {
        let mut s = State::new();
        s.toggle_at_cursor();
        let error = log::parse_line("E (1) app: boom");
        assert!(!s.is_visible(&error));
        let info = log::parse_line("I (1) app: ok");
        assert!(s.is_visible(&info));
    }

    #[test]
    fn toggle_all_hides_all_when_all_visible() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.toggle_all();
        assert!(s.is_level_hidden(log::Level::Error));
        assert!(s.is_level_hidden(log::Level::Info));
        assert!(s.is_tag_hidden("wifi"));
    }

    #[test]
    fn toggle_all_shows_all_when_any_hidden() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.toggle_at_cursor();
        s.toggle_all();
        assert!(!s.is_level_hidden(log::Level::Error));
        assert!(!s.is_tag_hidden("wifi"));
    }

    #[test]
    fn move_cursor_wraps() {
        let mut s = State::new();
        s.record_tag("a");
        s.record_tag("b");
        s.record_tag("c");
        let total = LEVELS.len() + 3;
        s.move_cursor(-1_isize);
        assert_eq!(s.cursor(), total - 1);
        s.move_cursor(1);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn known_tags_returns_all() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.record_tag("i2c");
        s.record_tag("esp_agent");
        assert_eq!(s.known_tags(), &["wifi", "i2c", "esp_agent"]);
    }

    #[test]
    fn known_tags_ignores_search_query() {
        let mut s = State::new();
        s.record_tag("WiFi");
        s.record_tag("i2c");
        s.record_tag("wifi_task");
        s.push_search_char('w');
        s.push_search_char('i');
        assert_eq!(s.known_tags(), &["WiFi", "i2c", "wifi_task"]);
    }

    #[test]
    fn push_and_pop_search_char() {
        let mut s = State::new();
        s.push_search_char('w');
        s.push_search_char('i');
        assert_eq!(s.search_query(), "wi");
        s.pop_search_char();
        assert_eq!(s.search_query(), "w");
        s.pop_search_char();
        assert_eq!(s.search_query(), "");
        s.pop_search_char();
        assert_eq!(s.search_query(), "");
    }

    #[test]
    fn search_persists_on_confirm() {
        let mut s = State::new();
        s.open_popup();
        s.push_search_char('w');
        assert_eq!(s.search_query(), "w");
        s.confirm_popup();
        assert!(!s.is_popup_open());
        assert_eq!(s.search_query(), "w");
    }

    #[test]
    fn cancel_popup_reverts_search_query() {
        let mut s = State::new();
        s.open_popup();
        s.push_search_char('w');
        s.push_search_char('i');
        s.cancel_popup();
        assert!(!s.is_popup_open());
        assert_eq!(s.search_query(), "");
        assert!(s.compiled_regex().is_none());
    }

    #[test]
    fn cancel_popup_reverts_hidden_levels() {
        let mut s = State::new();
        s.open_popup();
        s.toggle_at_cursor();
        assert!(s.is_level_hidden(log::Level::Error));
        s.cancel_popup();
        assert!(!s.is_level_hidden(log::Level::Error));
    }

    #[test]
    fn cancel_popup_reverts_hidden_tags() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.open_popup();
        s.move_cursor(LEVELS.len().cast_signed());
        s.toggle_at_cursor();
        assert!(s.is_tag_hidden("wifi"));
        s.cancel_popup();
        assert!(!s.is_tag_hidden("wifi"));
    }

    #[test]
    fn confirm_popup_keeps_changes() {
        let mut s = State::new();
        s.open_popup();
        s.toggle_at_cursor();
        s.push_search_char('x');
        s.confirm_popup();
        assert!(s.is_level_hidden(log::Level::Error));
        assert_eq!(s.search_query(), "x");
    }

    #[test]
    fn compiled_regex_none_when_query_empty() {
        let s = State::new();
        assert!(s.compiled_regex().is_none());
        assert!(!s.is_regex_error());
    }

    #[test]
    fn compiled_regex_some_for_valid_pattern() {
        let mut s = State::new();
        s.push_search_char('w');
        s.push_search_char('i');
        s.push_search_char('f');
        s.push_search_char('i');
        assert!(s.compiled_regex().is_some());
        assert!(!s.is_regex_error());
    }

    #[test]
    fn compiled_regex_none_and_error_for_invalid_pattern() {
        let mut s = State::new();
        s.push_search_char('[');
        assert!(s.compiled_regex().is_none());
        assert!(s.is_regex_error());
    }

    #[test]
    fn compiled_regex_is_case_insensitive() {
        let mut s = State::new();
        s.push_search_char('W');
        s.push_search_char('I');
        s.push_search_char('F');
        s.push_search_char('I');
        let re = s.compiled_regex().unwrap();
        assert!(re.is_match("wifi connected"));
        assert!(re.is_match("WIFI CONNECTED"));
    }

    #[test]
    fn compiled_regex_supports_alternation() {
        let mut s = State::new();
        for c in "wifi|i2c".chars() {
            s.push_search_char(c);
        }
        let re = s.compiled_regex().unwrap();
        assert!(re.is_match("wifi connected"));
        assert!(re.is_match("i2c timeout"));
        assert!(!re.is_match("uart ok"));
    }

    #[test]
    fn compiled_regex_clears_on_pop_to_empty() {
        let mut s = State::new();
        s.push_search_char('w');
        assert!(s.compiled_regex().is_some());
        s.pop_search_char();
        assert!(s.compiled_regex().is_none());
        assert!(!s.is_regex_error());
    }

    fn make_key(code: crossterm::event::KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::empty())
    }

    #[test]
    fn apply_search_key_returns_true_on_char_insert() {
        let mut s = State::new();
        assert!(s.apply_search_key(make_key(crossterm::event::KeyCode::Char('w'))));
    }

    #[test]
    fn apply_search_key_returns_true_on_backspace_when_nonempty() {
        let mut s = State::new();
        s.push_search_char('w');
        assert!(s.apply_search_key(make_key(crossterm::event::KeyCode::Backspace)));
    }

    #[test]
    fn apply_search_key_returns_false_on_backspace_at_empty() {
        let mut s = State::new();
        assert!(!s.apply_search_key(make_key(crossterm::event::KeyCode::Backspace)));
    }

    #[test]
    fn apply_search_key_returns_false_on_cursor_move() {
        let mut s = State::new();
        s.push_search_char('w');
        assert!(!s.apply_search_key(make_key(crossterm::event::KeyCode::Left)));
        assert!(!s.apply_search_key(make_key(crossterm::event::KeyCode::Right)));
    }

    #[test]
    fn popup_opens_with_search_focused() {
        let mut s = State::new();
        assert!(!s.is_search_focused());
        s.open_popup();
        assert!(s.is_search_focused());
        s.confirm_popup();
        assert!(!s.is_search_focused());
    }

    #[test]
    fn clear_search_resets_query_and_regex() {
        let mut s = State::new();
        s.push_search_char('w');
        s.push_search_char('i');
        assert_eq!(s.search_query(), "wi");
        assert!(s.compiled_regex().is_some());
        s.clear_search();
        assert_eq!(s.search_query(), "");
        assert!(s.compiled_regex().is_none());
        assert!(!s.is_regex_error());
    }
}

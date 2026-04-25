use std::collections::HashSet;

use crate::log;

/// Tracks which ESP-IDF tags are visible and manages the filter popup state.
pub struct State {
    known_tags: Vec<String>,
    hidden_tags: HashSet<String>,
    popup_open: bool,
    cursor: usize,
}

impl State {
    /// Creates a new empty filter state with no tags and the popup closed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            known_tags: Vec::new(),
            hidden_tags: HashSet::new(),
            popup_open: false,
            cursor: 0,
        }
    }

    /// Records a tag seen in the stream, adding it to the known list if new.
    ///
    /// # Arguments
    ///
    /// * `tag` - The ESP-IDF tag string to record.
    pub fn record_tag(&mut self, tag: &str) {
        if !tag.is_empty() && !self.known_tags.iter().any(|t| t == tag) {
            self.known_tags.push(tag.to_owned());
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
    /// `true` if the entry's tag is not hidden (or the entry has no tag).
    #[must_use]
    pub fn is_visible(&self, entry: &log::Entry) -> bool {
        !self.hidden_tags.contains(entry.tag())
    }

    /// Toggles the visibility of the tag at the current cursor position.
    pub fn toggle_at_cursor(&mut self) {
        let Some(tag) = self.known_tags.get(self.cursor).cloned() else {
            return;
        };
        if self.hidden_tags.contains(&tag) {
            self.hidden_tags.remove(&tag);
        } else {
            self.hidden_tags.insert(tag);
        }
    }

    /// Moves the cursor by `delta` positions, clamped to the tag list bounds.
    ///
    /// # Arguments
    ///
    /// * `delta` - Positive to move down, negative to move up.
    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.known_tags.len();
        if len == 0 {
            return;
        }
        self.cursor = self.cursor.saturating_add_signed(delta).min(len - 1);
    }

    /// Clears all hidden tags, making every tag visible.
    pub fn clear_hidden(&mut self) {
        self.hidden_tags.clear();
    }

    /// Toggles the filter popup open or closed.
    pub fn toggle_popup(&mut self) {
        self.popup_open = !self.popup_open;
    }

    /// Returns whether the filter popup is currently open.
    #[must_use]
    pub fn is_popup_open(&self) -> bool {
        self.popup_open
    }

    /// Returns all tags seen so far, in insertion order.
    #[must_use]
    pub fn known_tags(&self) -> &[String] {
        &self.known_tags
    }

    /// Returns whether the given tag is currently hidden.
    ///
    /// # Arguments
    ///
    /// * `tag` - The tag name to check.
    #[must_use]
    pub fn is_tag_hidden(&self, tag: &str) -> bool {
        self.hidden_tags.contains(tag)
    }

    /// Returns the current cursor index within the known tags list.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log;

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
    fn toggle_hides_and_shows_tag() {
        let mut s = State::new();
        s.record_tag("wifi");
        assert!(!s.is_tag_hidden("wifi"));
        s.toggle_at_cursor();
        assert!(s.is_tag_hidden("wifi"));
        s.toggle_at_cursor();
        assert!(!s.is_tag_hidden("wifi"));
    }

    #[test]
    fn is_visible_respects_hidden() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.toggle_at_cursor();
        let entry = log::parse_line("I (1) wifi: msg");
        assert!(!s.is_visible(&entry));
        let other = log::parse_line("I (1) i2c: msg");
        assert!(s.is_visible(&other));
    }

    #[test]
    fn clear_hidden_restores_all() {
        let mut s = State::new();
        s.record_tag("wifi");
        s.toggle_at_cursor();
        s.clear_hidden();
        assert!(!s.is_tag_hidden("wifi"));
    }

    #[test]
    fn move_cursor_clamps() {
        let mut s = State::new();
        s.record_tag("a");
        s.record_tag("b");
        s.record_tag("c");
        s.move_cursor(-5_isize);
        assert_eq!(s.cursor(), 0);
        s.move_cursor(100);
        assert_eq!(s.cursor(), 2);
    }
}

/// State for the port selection popup shown when multiple ports are detected.
pub(crate) struct Selector {
    ports: Vec<String>,
    cursor: usize,
}

impl Selector {
    /// Creates a new port selector with the given list of candidate ports.
    ///
    /// # Arguments
    ///
    /// * `ports` - Non-empty list of port names to select from.
    #[must_use]
    pub(crate) fn new(ports: Vec<String>) -> Self {
        Self { ports, cursor: 0 }
    }

    /// Returns all candidate port names.
    ///
    /// # Returns
    ///
    /// A slice of port name strings in selection order.
    #[must_use]
    pub(crate) fn ports(&self) -> &[String] {
        &self.ports
    }

    /// Returns the current cursor index.
    ///
    /// # Returns
    ///
    /// Zero-based index into the port list.
    #[must_use]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Moves the cursor by `delta`, clamped to the port list bounds.
    ///
    /// # Arguments
    ///
    /// * `delta` - Positive to move down, negative to move up.
    pub(crate) fn move_cursor(&mut self, delta: isize) {
        let len = self.ports.len();
        if len > 0 {
            self.cursor = self.cursor.saturating_add_signed(delta).min(len - 1);
        }
    }

    /// Returns the currently selected port name.
    ///
    /// # Returns
    ///
    /// The port name at the current cursor, or an empty string if the list is
    /// empty.
    #[must_use]
    pub(crate) fn selected(&self) -> &str {
        self.ports.get(self.cursor).map_or("", String::as_str)
    }

    /// Replaces the candidate port list and clamps the cursor to the new
    /// bounds.
    ///
    /// # Arguments
    ///
    /// * `ports` - Updated list of available ports.
    pub(crate) fn update_ports(&mut self, ports: Vec<String>) {
        self.cursor = self.cursor.min(ports.len().saturating_sub(1));
        self.ports = ports;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_cursor() {
        let sel = Selector::new(vec!["COM1".into(), "COM2".into()]);
        assert_eq!(sel.cursor(), 0);
        assert_eq!(sel.selected(), "COM1");
    }

    #[test]
    fn move_cursor_navigation() {
        let mut sel =
            Selector::new(vec!["COM1".into(), "COM2".into(), "COM3".into()]);
        sel.move_cursor(1);
        assert_eq!(sel.cursor(), 1);
        assert_eq!(sel.selected(), "COM2");
        sel.move_cursor(-1);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn move_cursor_clamps() {
        let mut sel = Selector::new(vec!["COM1".into(), "COM2".into()]);
        sel.move_cursor(-10);
        assert_eq!(sel.cursor(), 0);
        sel.move_cursor(100);
        assert_eq!(sel.cursor(), 1);
    }

    #[test]
    fn move_cursor_empty_list() {
        let mut sel = Selector::new(vec![]);
        sel.move_cursor(1);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn selected_empty() {
        let sel = Selector::new(vec![]);
        assert_eq!(sel.selected(), "");
    }

    #[test]
    fn update_ports_replaces_list_and_clamps_cursor() {
        let mut sel =
            Selector::new(vec!["COM1".into(), "COM2".into(), "COM3".into()]);
        sel.move_cursor(2);
        sel.update_ports(vec!["COM4".into()]);
        assert_eq!(sel.ports(), &["COM4"]);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn update_ports_empty_resets_cursor() {
        let mut sel = Selector::new(vec!["COM1".into()]);
        sel.update_ports(vec![]);
        assert_eq!(sel.cursor(), 0);
        assert!(sel.ports().is_empty());
    }
}

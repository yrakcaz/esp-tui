use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::watch;

use esp_agent_msg as agent_msg;

use crate::{config, elf, filter, flash, log, port, serial};

pub(crate) const DEFAULT_BAUD: u32 = 115_200;
const STATUS_TTL_SECS: u64 = 3;
// Sentinel clamped by visible_entries to total.saturating_sub(height), i.e. oldest window.
const SCROLL_TOP: usize = usize::MAX;

/// Every action that can be bound to a key.
///
/// Navigation variants are handled inline in [`App::apply_keymap`]; all others
/// are converted to [`Action`] and returned to the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MappableAction {
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ScrollTop,
    ScrollBottom,
    SwitchPane,
    GrowMonitor,
    ShrinkMonitor,
    ToggleFilter,
    ClearLog,
    Quit,
    QuitPrompt,
    Flash,
    ErasePrompt,
    ResetDevice,
    Disconnect,
    ScanPorts,
}

pub(crate) type KeyMap = HashMap<(KeyCode, KeyModifiers), MappableAction>;

pub(crate) fn default_keymap() -> KeyMap {
    let none = KeyModifiers::empty();
    let ctrl = KeyModifiers::CONTROL;
    [
        ((KeyCode::Char('q'), none), MappableAction::QuitPrompt),
        ((KeyCode::Esc, none), MappableAction::QuitPrompt),
        ((KeyCode::Char('d'), none), MappableAction::Disconnect),
        ((KeyCode::Char('r'), none), MappableAction::ResetDevice),
        ((KeyCode::Char('f'), ctrl), MappableAction::ToggleFilter),
        ((KeyCode::Char('f'), none), MappableAction::Flash),
        ((KeyCode::Char('e'), none), MappableAction::ErasePrompt),
        ((KeyCode::Char('c'), none), MappableAction::ScanPorts),
        ((KeyCode::Tab, none), MappableAction::SwitchPane),
        ((KeyCode::Right, ctrl), MappableAction::GrowMonitor),
        ((KeyCode::Left, ctrl), MappableAction::ShrinkMonitor),
        ((KeyCode::Char('l'), ctrl), MappableAction::ClearLog),
        ((KeyCode::Up, none), MappableAction::ScrollUp),
        ((KeyCode::Down, none), MappableAction::ScrollDown),
        ((KeyCode::PageUp, none), MappableAction::PageUp),
        ((KeyCode::PageDown, none), MappableAction::PageDown),
    ]
    .into_iter()
    .collect()
}

/// Formats a key as a short display string for use in hints.
///
/// # Arguments
///
/// * `code` - The key code.
/// * `mods` - The key modifiers.
///
/// # Returns
///
/// A string such as `"F"`, `"^F"`, `"↑"`, `"Tab"`, `"PgUp"`.
pub(crate) fn format_key_display(code: KeyCode, mods: KeyModifiers) -> String {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let prefix: &str = match (ctrl, alt) {
        (true, true) => "^M-",
        (true, false) => "^",
        (false, true) => "M-",
        (false, false) => "",
    };
    match code {
        KeyCode::Char(c) => format!("{}{}", prefix, c.to_ascii_uppercase()),
        KeyCode::Up => format!("{prefix}↑"),
        KeyCode::Down => format!("{prefix}↓"),
        KeyCode::Left => format!("{prefix}←"),
        KeyCode::Right => format!("{prefix}→"),
        KeyCode::PageUp => format!("{prefix}PgUp"),
        KeyCode::PageDown => format!("{prefix}PgDn"),
        KeyCode::Tab => "Tab".to_owned(),
        KeyCode::BackTab => "⇧Tab".to_owned(),
        KeyCode::Enter => "Enter".to_owned(),
        KeyCode::Esc => "Esc".to_owned(),
        KeyCode::Backspace => "Bksp".to_owned(),
        KeyCode::Delete => "Del".to_owned(),
        KeyCode::Home => "Home".to_owned(),
        KeyCode::End => "End".to_owned(),
        KeyCode::F(n) => format!("F{n}"),
        _ => "?".to_owned(),
    }
}

fn pick_best_key(keys: &[(KeyCode, KeyModifiers)]) -> (KeyCode, KeyModifiers) {
    keys.iter()
        .min_by_key(|(code, mods)| {
            let priority: u8 = match (code, mods.is_empty()) {
                (KeyCode::Char(_), true) => 0,
                (_, true) => 1,
                _ => 2,
            };
            (priority, format!("{code:?}{mods:?}"))
        })
        .copied()
        .expect("pick_best_key called with non-empty slice")
}

fn parse_action(s: &str) -> Option<MappableAction> {
    match s {
        "scroll_up" => Some(MappableAction::ScrollUp),
        "scroll_down" => Some(MappableAction::ScrollDown),
        "page_up" => Some(MappableAction::PageUp),
        "page_down" => Some(MappableAction::PageDown),
        "scroll_top" => Some(MappableAction::ScrollTop),
        "scroll_bottom" => Some(MappableAction::ScrollBottom),
        "switch_pane" => Some(MappableAction::SwitchPane),
        "grow_monitor" => Some(MappableAction::GrowMonitor),
        "shrink_monitor" => Some(MappableAction::ShrinkMonitor),
        "toggle_filter" => Some(MappableAction::ToggleFilter),
        "clear_log" => Some(MappableAction::ClearLog),
        "quit" => Some(MappableAction::Quit),
        "quit_prompt" => Some(MappableAction::QuitPrompt),
        "flash" => Some(MappableAction::Flash),
        "erase_prompt" => Some(MappableAction::ErasePrompt),
        "reset_device" => Some(MappableAction::ResetDevice),
        "disconnect" => Some(MappableAction::Disconnect),
        "scan_ports" => Some(MappableAction::ScanPorts),
        _ => None,
    }
}

fn build_keymap(keys: &config::KeysConfig) -> KeyMap {
    let mut map = default_keymap();

    let insert = |map: &mut KeyMap, k: &str, v: &str| {
        if let (Ok(key), Some(action)) = (config::parse_key(k), parse_action(v)) {
            map.retain(|_, a| *a != action);
            map.insert(key, action);
        }
    };

    if let Some(preset) = &keys.preset {
        if let Ok(overrides) = config::load_preset_overrides(preset) {
            for (k, v) in overrides {
                insert(&mut map, &k, &v);
            }
        }
    }

    for (k, v) in &keys.overrides {
        insert(&mut map, k, v);
    }

    map
}

/// Outcome of a keypress that requires I/O, returned to the event loop to act on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// No I/O required; state was updated in place or key was ignored.
    None,
    /// Shut down the application.
    Quit,
    /// Send a hardware reset pulse to the connected ESP32.
    ResetDevice,
    /// Close the active serial connection.
    Disconnect,
    /// Scan for available serial ports and connect or open the selector.
    ScanPorts,
    /// Connect to the given port name (emitted by the port selector popup).
    ConnectPort(String),
    /// Start flashing the selected ELF to the connected device.
    Flash,
    /// Open the erase confirmation prompt.
    ErasePrompt,
    /// Confirm the erase and start the operation.
    ConfirmErase,
    /// Close the ELF path selector popup without saving.
    CloseElfSelector,
    /// Confirm the ELF path currently typed in the selector.
    ConfirmElfPath,
    /// Open the quit confirmation prompt.
    QuitPrompt,
}

/// Which pane currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    /// Serial monitor log pane.
    Monitor,
    /// System inspector pane.
    Inspector,
    /// Status bar (flash progress / status messages).
    Status,
}

enum ConfirmDialog {
    None,
    Quit,
    Erase,
}

fn is_modal_safe_key(key: KeyEvent) -> bool {
    !matches!(key.code, KeyCode::Char(_)) || !key.modifiers.is_empty()
}

fn push_history(history: &mut VecDeque<u32>, val: u32, max_len: usize) {
    history.push_back(val);
    if history.len() > max_len {
        history.pop_front();
    }
}

fn matches_search(entry: &log::Entry, query: &str) -> bool {
    query.is_empty()
        || entry.message().to_lowercase().contains(query)
        || entry.tag().to_lowercase().contains(query)
}

/// Central application state.
pub(crate) struct App {
    config: config::Config,
    keymap: KeyMap,
    log_buffer: VecDeque<log::Entry>,
    scroll: usize,
    inspector_scroll: usize,
    inspector_max_scroll: Cell<usize>,
    focused_pane: Pane,
    monitor_pct: u16,
    filter: filter::State,
    port_name: Option<String>,
    port_cmd_tx: Option<std::sync::mpsc::Sender<serial::PortCommand>>,
    source_shutdown_tx: Option<watch::Sender<bool>>,
    status_msg: Option<(String, Instant)>,
    running: bool,
    port_selector: Option<port::Selector>,
    flash_state: flash::State,
    device_info: Option<flash::DeviceInfo>,
    confirm: ConfirmDialog,
    elf_path: Option<PathBuf>,
    elf_selector: Option<elf::Selector>,
    baud: u32,
    agent_frame: Option<agent_msg::Frame>,
    agent_startup: Option<agent_msg::Startup>,
    agent_partitions:
        Option<heapless::Vec<agent_msg::Partition, { agent_msg::MAX_PARTITIONS }>>,
    agent_last_seen: Option<Instant>,
    connected_at: Option<Instant>,
    heap_history: VecDeque<u32>,
    cpu_history: [VecDeque<u32>; 2],
}

impl App {
    /// Creates a new application state.
    ///
    /// # Arguments
    ///
    /// * `port_name` - The connected serial port name, if already known.
    /// * `config` - Loaded configuration; determines colors, key bindings, and
    ///   buffer sizes.
    ///
    /// # Returns
    ///
    /// An [`App`] with an empty log buffer, all filters visible, and the event
    /// loop running.
    #[must_use]
    pub(crate) fn new(port_name: Option<String>, config: config::Config) -> Self {
        let keymap = build_keymap(&config.keys);
        Self {
            config,
            keymap,
            log_buffer: VecDeque::new(),
            scroll: 0,
            inspector_scroll: 0,
            inspector_max_scroll: Cell::new(0),
            focused_pane: Pane::Monitor,
            monitor_pct: 60,
            filter: filter::State::new(),
            port_name,
            port_cmd_tx: None,
            source_shutdown_tx: None,
            status_msg: None,
            running: true,
            port_selector: None,
            flash_state: flash::State::Idle,
            device_info: None,
            confirm: ConfirmDialog::None,
            elf_path: None,
            elf_selector: None,
            baud: DEFAULT_BAUD,
            agent_frame: None,
            agent_startup: None,
            agent_partitions: None,
            agent_last_seen: None,
            connected_at: None,
            heap_history: VecDeque::new(),
            cpu_history: [VecDeque::new(), VecDeque::new()],
        }
    }

    /// Pushes a raw serial line into the log buffer, parsing it and evicting
    /// the oldest entry when the buffer is full.
    ///
    /// # Arguments
    ///
    /// * `line` - A single line of serial output.
    pub(crate) fn push_line(&mut self, line: &str) {
        if !line.trim().is_empty() {
            let entry = log::parse_line(line);
            self.filter.record_tag(entry.tag());
            if entry.tag() == agent_msg::TAG {
                self.agent_last_seen = Some(Instant::now());
                match agent_msg::parse::parse(entry.timestamp_ms(), entry.message())
                {
                    Some(agent_msg::Message::Frame(f)) => {
                        self.inspector_scroll = self
                            .inspector_scroll
                            .min(self.inspector_max_scroll.get());
                        push_history(
                            &mut self.heap_history,
                            f.heap_free,
                            self.config.ui.sparkline_len,
                        );
                        f.cpu_usage.iter().enumerate().for_each(|(i, &usage)| {
                            push_history(
                                &mut self.cpu_history[i],
                                u32::from(usage),
                                self.config.ui.sparkline_len,
                            );
                        });
                        self.agent_frame = Some(f);
                    }
                    Some(agent_msg::Message::Startup(s)) => {
                        self.agent_startup = Some(s);
                    }
                    Some(agent_msg::Message::Partitions(p)) => {
                        self.agent_partitions = Some(p);
                    }
                    None => {}
                }
            }
            if self.log_buffer.len() >= self.config.ui.buffer_size {
                self.log_buffer.pop_front();
            }
            let query = self.filter.search_query().to_lowercase();
            if self.scroll > 0
                && self.filter.is_visible(&entry)
                && matches_search(&entry, &query)
            {
                self.scroll = self.scroll.saturating_add(1);
            }
            self.log_buffer.push_back(entry);
        }
    }

    /// Handles a keypress and returns the action the event loop should perform.
    ///
    /// # Arguments
    ///
    /// * `key` - The key event to handle.
    ///
    /// # Returns
    ///
    /// An [`Action`] indicating what I/O the event loop should perform.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            Action::Quit
        } else if matches!(self.confirm, ConfirmDialog::Quit) {
            self.handle_key_quit_confirm(key)
        } else if matches!(self.confirm, ConfirmDialog::Erase) {
            self.handle_key_erase_confirm(key)
        } else if self.elf_selector.is_some() {
            self.handle_key_elf_selector(key)
        } else if self.port_selector.is_some() {
            self.handle_key_port_selector(key)
        } else if self.filter.is_popup_open() {
            self.handle_key_filter_popup(key);
            Action::None
        } else {
            self.handle_key_normal(key)
        }
    }

    fn mapped_to(&self, key: KeyEvent, action: MappableAction) -> bool {
        self.keymap.get(&(key.code, key.modifiers)) == Some(&action)
    }

    fn handle_key_quit_confirm(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Char('y') {
            Action::Quit
        } else if key.code == KeyCode::Char('n')
            || key.code == KeyCode::Esc
            || self.mapped_to(key, MappableAction::QuitPrompt)
        {
            self.close_quit_confirm();
            Action::None
        } else {
            Action::None
        }
    }

    fn handle_key_erase_confirm(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Char('y') {
            Action::ConfirmErase
        } else if key.code == KeyCode::Char('n')
            || key.code == KeyCode::Esc
            || self.mapped_to(key, MappableAction::ErasePrompt)
            || self.mapped_to(key, MappableAction::QuitPrompt)
        {
            self.confirm = ConfirmDialog::None;
            Action::None
        } else {
            Action::None
        }
    }

    fn handle_key_elf_selector(&mut self, key: KeyEvent) -> Action {
        // For text-input modals, only look up the keymap for non-printable
        // keys (arrows, Esc, modifier combos) so that plain chars still type.
        let safe = is_modal_safe_key(key);
        if key.code == KeyCode::Esc
            || (safe && self.mapped_to(key, MappableAction::QuitPrompt))
            || (safe && self.mapped_to(key, MappableAction::Flash))
        {
            return Action::CloseElfSelector;
        }
        if safe && self.mapped_to(key, MappableAction::ScrollUp) {
            if let Some(s) = self.elf_selector.as_mut() {
                s.move_completion(-1);
            }
            return Action::None;
        }
        if safe && self.mapped_to(key, MappableAction::ScrollDown) {
            if let Some(s) = self.elf_selector.as_mut() {
                s.move_completion(1);
            }
            return Action::None;
        }
        match key.code {
            KeyCode::Enter => {
                let was_cycling = self
                    .elf_selector
                    .as_ref()
                    .is_some_and(|s| !s.completions().is_empty());
                if let Some(s) = self.elf_selector.as_mut() {
                    s.accept_completion();
                }
                if was_cycling {
                    Action::None
                } else {
                    Action::ConfirmElfPath
                }
            }
            KeyCode::Tab => {
                if let Some(s) = self.elf_selector.as_mut() {
                    s.tab_complete();
                }
                Action::None
            }
            KeyCode::BackTab => {
                if let Some(s) = self.elf_selector.as_mut() {
                    s.cycle_completion_back();
                }
                Action::None
            }
            _ => {
                if let Some(s) = self.elf_selector.as_mut() {
                    s.apply_key(key);
                }
                Action::None
            }
        }
    }

    fn handle_key_port_selector(&mut self, key: KeyEvent) -> Action {
        let cancel = self.mapped_to(key, MappableAction::ScanPorts)
            || self.mapped_to(key, MappableAction::QuitPrompt)
            || key.code == KeyCode::Esc;
        if cancel {
            self.port_selector = None;
            Action::None
        } else if self.mapped_to(key, MappableAction::ScrollUp) {
            if let Some(s) = self.port_selector.as_mut() {
                s.move_cursor(-1);
            }
            Action::None
        } else if self.mapped_to(key, MappableAction::ScrollDown) {
            if let Some(s) = self.port_selector.as_mut() {
                s.move_cursor(1);
            }
            Action::None
        } else {
            match key.code {
                KeyCode::Enter => {
                    self.port_selector.take().map_or(Action::None, |s| {
                        Action::ConnectPort(s.selected().to_owned())
                    })
                }
                _ => Action::None,
            }
        }
    }

    fn handle_key_filter_popup(&mut self, key: KeyEvent) {
        let safe = is_modal_safe_key(key);
        if self.filter.is_search_focused() {
            if key.code == KeyCode::Esc
                || (safe && self.mapped_to(key, MappableAction::QuitPrompt))
            {
                self.filter.unfocus_search();
            } else if key.code == KeyCode::Up {
                self.filter.unfocus_search();
                self.filter.move_cursor(-1);
            } else if key.code == KeyCode::Down {
                self.filter.unfocus_search();
                self.filter.move_cursor(1);
            } else {
                self.filter.apply_search_key(key);
            }
        } else if key.code == KeyCode::Esc
            || (safe && self.mapped_to(key, MappableAction::QuitPrompt))
            || self.mapped_to(key, MappableAction::ToggleFilter)
        {
            self.filter.toggle_popup();
        } else if key.code == KeyCode::Up {
            if self.filter.cursor() == 0 {
                self.filter.focus_search();
            } else {
                self.filter.move_cursor(-1);
            }
        } else if key.code == KeyCode::Down {
            self.filter.move_cursor(1);
        } else {
            match key.code {
                KeyCode::Char(' ')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.filter.toggle_at_cursor();
                }
                KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
                    self.filter.toggle_all();
                }
                KeyCode::Backspace => {
                    self.filter.focus_search();
                    self.filter.apply_search_key(key);
                }
                KeyCode::Char(_)
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.filter.focus_search();
                    self.filter.apply_search_key(key);
                }
                _ => {}
            }
        }
    }

    fn scroll_active_pane_up(&mut self, amount: usize) {
        match self.focused_pane {
            Pane::Monitor => {
                self.scroll = self.scroll.saturating_add(amount);
            }
            Pane::Inspector => {
                self.inspector_scroll = self.inspector_scroll.saturating_sub(amount);
            }
            Pane::Status => {}
        }
    }

    fn scroll_active_pane_down(&mut self, amount: usize) {
        match self.focused_pane {
            Pane::Monitor => {
                self.scroll = self.scroll.saturating_sub(amount);
            }
            Pane::Inspector => {
                self.inspector_scroll = self
                    .inspector_scroll
                    .saturating_add(amount)
                    .min(self.inspector_max_scroll.get());
            }
            Pane::Status => {}
        }
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> Action {
        self.apply_keymap(key)
    }

    fn apply_keymap(&mut self, key: KeyEvent) -> Action {
        match self.keymap.get(&(key.code, key.modifiers)).copied() {
            Some(MappableAction::ScrollUp) => {
                self.scroll_active_pane_up(1);
                Action::None
            }
            Some(MappableAction::ScrollDown) => {
                self.scroll_active_pane_down(1);
                Action::None
            }
            Some(MappableAction::PageUp) => {
                self.scroll_active_pane_up(10);
                Action::None
            }
            Some(MappableAction::PageDown) => {
                self.scroll_active_pane_down(10);
                Action::None
            }
            Some(MappableAction::ScrollTop) => {
                match self.focused_pane {
                    Pane::Monitor => self.scroll = SCROLL_TOP,
                    Pane::Inspector => self.inspector_scroll = 0,
                    Pane::Status => {}
                }
                Action::None
            }
            Some(MappableAction::ScrollBottom) => {
                match self.focused_pane {
                    Pane::Monitor => self.scroll = 0,
                    Pane::Inspector => {
                        self.inspector_scroll = self.inspector_max_scroll.get();
                    }
                    Pane::Status => {}
                }
                Action::None
            }
            Some(MappableAction::SwitchPane) => {
                self.focused_pane = match self.focused_pane {
                    Pane::Monitor => {
                        self.monitor_pct = self.monitor_pct.min(80);
                        Pane::Inspector
                    }
                    Pane::Inspector => {
                        self.monitor_pct = self.monitor_pct.max(20);
                        Pane::Monitor
                    }
                    Pane::Status => Pane::Monitor,
                };
                Action::None
            }
            Some(MappableAction::GrowMonitor) => {
                self.grow_monitor();
                if self.focused_pane == Pane::Inspector && self.monitor_pct == 100 {
                    self.focused_pane = Pane::Monitor;
                }
                Action::None
            }
            Some(MappableAction::ShrinkMonitor) => {
                self.shrink_monitor();
                if self.focused_pane == Pane::Monitor && self.monitor_pct == 0 {
                    self.focused_pane = Pane::Inspector;
                }
                Action::None
            }
            Some(MappableAction::ToggleFilter) => {
                if self.focused_pane == Pane::Monitor {
                    self.filter.toggle_popup();
                }
                Action::None
            }
            Some(MappableAction::ClearLog) => {
                if self.focused_pane == Pane::Monitor {
                    self.clear_log();
                }
                Action::None
            }
            Some(MappableAction::Quit) => Action::Quit,
            Some(MappableAction::QuitPrompt) => {
                if self.focused_pane == Pane::Monitor && self.scroll > 0 {
                    self.scroll = 0;
                    Action::None
                } else if self.focused_pane == Pane::Inspector
                    && self.inspector_scroll > 0
                {
                    self.inspector_scroll = 0;
                    Action::None
                } else {
                    Action::QuitPrompt
                }
            }
            Some(MappableAction::Flash) => Action::Flash,
            Some(MappableAction::ErasePrompt) => Action::ErasePrompt,
            Some(MappableAction::ResetDevice) => Action::ResetDevice,
            Some(MappableAction::Disconnect) => Action::Disconnect,
            Some(MappableAction::ScanPorts) => Action::ScanPorts,
            None => Action::None,
        }
    }

    /// Sets an ephemeral status message that expires after a few seconds.
    ///
    /// # Arguments
    ///
    /// * `msg` - The message to display in the status bar.
    pub(crate) fn set_status(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    /// Returns whether the application event loop should keep running.
    ///
    /// # Returns
    ///
    /// `true` until [`Self::quit`] is called.
    #[must_use]
    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    /// Returns the connected serial port name, if any.
    ///
    /// # Returns
    ///
    /// `Some` with the port name string, or `None` if no port is connected.
    #[must_use]
    pub(crate) fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }

    /// Returns the current status message text, if any.
    ///
    /// # Returns
    ///
    /// `Some` with the message string, or `None` if no message is active.
    #[must_use]
    pub(crate) fn status_msg(&self) -> Option<&str> {
        self.status_msg.as_ref().map(|(msg, _)| msg.as_str())
    }

    /// Returns a shared reference to the filter state.
    ///
    /// # Returns
    ///
    /// A reference to the current [`filter::State`].
    #[must_use]
    pub(crate) fn filter(&self) -> &filter::State {
        &self.filter
    }

    /// Returns a mutable reference to the filter state.
    ///
    /// # Returns
    ///
    /// A mutable reference to the current [`filter::State`].
    #[cfg(test)]
    pub(crate) fn filter_mut(&mut self) -> &mut filter::State {
        &mut self.filter
    }

    /// Returns how many lines from the bottom are scrolled out of view.
    /// Zero means auto-scroll (pinned to the latest line).
    ///
    /// # Returns
    ///
    /// Number of visible lines currently scrolled above the bottom of the pane.
    #[must_use]
    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    /// Returns the log entries visible within a pane of the given height,
    /// respecting the current filter and scroll offset.
    ///
    /// # Arguments
    ///
    /// * `height` - The number of lines the pane can display.
    ///
    /// # Returns
    ///
    /// A `Vec` of references to visible entries, oldest first.
    #[must_use]
    pub(crate) fn visible_entries(&self, height: usize) -> Vec<&log::Entry> {
        let query = self.filter.search_query().to_lowercase();
        let visible: Vec<&log::Entry> = self
            .log_buffer
            .iter()
            .filter(|e| self.filter.is_visible(e) && matches_search(e, &query))
            .collect();
        let total = visible.len();
        let skip = self.scroll.min(total.saturating_sub(height));
        let start = total.saturating_sub(height).saturating_sub(skip);
        visible.into_iter().skip(start).take(height).collect()
    }

    /// Returns a shared reference to the port selector, if active.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the active [`port::Selector`], or `None` if
    /// no selector is open.
    #[must_use]
    pub(crate) fn port_selector(&self) -> Option<&port::Selector> {
        self.port_selector.as_ref()
    }

    /// Returns a mutable reference to the port selector, if active.
    ///
    /// # Returns
    ///
    /// `Some` with a mutable reference to the active [`port::Selector`], or
    /// `None` if no selector is open.
    #[cfg(test)]
    pub(crate) fn port_selector_mut(&mut self) -> Option<&mut port::Selector> {
        self.port_selector.as_mut()
    }

    /// Returns a mutable reference to the ELF selector, if open.
    ///
    /// # Returns
    ///
    /// `Some` with a mutable reference to the active [`elf::Selector`], or
    /// `None` if no selector is open.
    #[cfg(test)]
    pub(crate) fn elf_selector_mut(&mut self) -> Option<&mut elf::Selector> {
        self.elf_selector.as_mut()
    }

    /// Sets the connected port name and clears the port selector.
    ///
    /// # Arguments
    ///
    /// * `port` - The port name to use going forward.
    pub(crate) fn set_port(&mut self, port: String) {
        self.port_name = Some(port);
        self.port_selector = None;
        self.port_cmd_tx = None;
        self.connected_at = Some(Instant::now());
    }

    /// Stores the command sender for the currently connected port reader task.
    ///
    /// # Arguments
    ///
    /// * `tx` - Sender returned by [`serial::Port::spawn`].
    pub(crate) fn set_port_cmd(
        &mut self,
        tx: std::sync::mpsc::Sender<serial::PortCommand>,
    ) {
        self.port_cmd_tx = Some(tx);
    }

    /// Returns the command sender for the active port reader, if any.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the sender, or `None` if no port is
    /// connected.
    #[must_use]
    pub(crate) fn port_cmd_tx(
        &self,
    ) -> Option<&std::sync::mpsc::Sender<serial::PortCommand>> {
        self.port_cmd_tx.as_ref()
    }

    /// Registers a shutdown sender for the active data source.
    ///
    /// If a previous source is still registered, it is stopped by sending
    /// `true` before the new sender is stored.
    ///
    /// # Arguments
    ///
    /// * `tx` - Watch sender for the new source's shutdown channel.
    pub(crate) fn set_source_shutdown(&mut self, tx: watch::Sender<bool>) {
        if let Some(old) = self.source_shutdown_tx.replace(tx) {
            let _ = old.send(true);
        }
    }

    /// Stops the active data source, if any.
    pub(crate) fn shutdown_source(&mut self) {
        if let Some(tx) = self.source_shutdown_tx.take() {
            let _ = tx.send(true);
        }
    }

    /// Activates the port selector popup with the given candidate ports.
    ///
    /// # Arguments
    ///
    /// * `ports` - Non-empty list of port names to present for selection.
    pub(crate) fn open_port_selector(&mut self, ports: Vec<String>) {
        self.port_selector = Some(port::Selector::new(ports));
    }

    /// Updates the open port selector with a refreshed port list.
    ///
    /// Closes the selector when `ports` is empty; otherwise replaces the list
    /// and clamps the cursor.
    ///
    /// # Arguments
    ///
    /// * `ports` - Updated list of available ports.
    pub(crate) fn refresh_port_selector(&mut self, ports: Vec<String>) {
        if ports.is_empty() {
            self.close_port_selector();
        } else if let Some(sel) = self.port_selector.as_mut() {
            sel.update_ports(ports);
        }
    }

    /// Closes the port selector popup, if open.
    pub(crate) fn close_port_selector(&mut self) {
        self.port_selector = None;
    }

    /// Signals the event loop to stop.
    pub(crate) fn quit(&mut self) {
        self.running = false;
    }

    /// Clears the log buffer and resets the scroll offset to zero.
    pub(crate) fn clear_log(&mut self) {
        self.log_buffer.clear();
        self.scroll = 0;
    }

    /// Expires the status message if its TTL has elapsed. Called on each tick.
    pub(crate) fn tick(&mut self) {
        if let Some((_, ts)) = &self.status_msg {
            if ts.elapsed().as_secs() >= STATUS_TTL_SECS {
                self.status_msg = None;
            }
        }
    }

    /// Tears down the active port connection and clears port state.
    pub(crate) fn disconnect(&mut self) {
        self.shutdown_source();
        self.port_name = None;
        self.port_cmd_tx = None;
        self.device_info = None;
        self.clear_agent_data();
    }

    /// Clears all agent telemetry fields and resets the connection timestamp.
    ///
    /// Called on every new connection so stale telemetry from a previous
    /// firmware image is never shown alongside data from a new one.
    pub(crate) fn clear_agent_data(&mut self) {
        self.agent_frame = None;
        self.agent_startup = None;
        self.agent_partitions = None;
        self.agent_last_seen = None;
        self.connected_at = None;
        self.heap_history.clear();
        self.cpu_history.iter_mut().for_each(VecDeque::clear);
    }

    /// Returns the heap free history for the sparkline, oldest value first.
    ///
    /// # Returns
    ///
    /// A reference to the ring buffer of recent `heap_free` samples.
    #[must_use]
    pub(crate) fn heap_history(&self) -> &VecDeque<u32> {
        &self.heap_history
    }

    /// Returns per-core CPU usage history for the sparkline, oldest first.
    ///
    /// # Returns
    ///
    /// A reference to the two-element array of CPU usage sample buffers;
    /// index 0 is core 0, index 1 is core 1.
    #[must_use]
    pub(crate) fn cpu_history(&self) -> &[VecDeque<u32>; 2] {
        &self.cpu_history
    }

    /// Returns the current flash operation state.
    ///
    /// # Returns
    ///
    /// A reference to the current [`flash::State`].
    #[must_use]
    pub(crate) fn flash_state(&self) -> &flash::State {
        &self.flash_state
    }

    /// Returns `true` while a flash or erase operation is in progress or the
    /// device is reconnecting after one.
    ///
    /// # Returns
    ///
    /// `true` if state is `Flashing`, `Erasing`, or `Reconnecting`.
    #[must_use]
    pub(crate) fn is_flashing(&self) -> bool {
        matches!(
            self.flash_state,
            flash::State::Flashing { .. }
                | flash::State::Erasing
                | flash::State::Reconnecting
        )
    }

    /// Updates the flash operation state.
    ///
    /// # Arguments
    ///
    /// * `state` - The new [`flash::State`].
    pub(crate) fn set_flash_state(&mut self, state: flash::State) {
        self.flash_state = state;
    }

    /// Returns the device info received after the last successful connection.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to [`flash::DeviceInfo`], or `None` if no info
    /// has been received.
    #[must_use]
    pub(crate) fn device_info(&self) -> Option<&flash::DeviceInfo> {
        self.device_info.as_ref()
    }

    /// Stores device info received from the probe task.
    ///
    /// # Arguments
    ///
    /// * `info` - The [`flash::DeviceInfo`] returned by the probe.
    pub(crate) fn set_device_info(&mut self, info: flash::DeviceInfo) {
        self.device_info = Some(info);
    }

    /// Returns `true` while the erase confirmation prompt is visible.
    ///
    /// # Returns
    ///
    /// `true` if the erase confirm dialog is open, `false` otherwise.
    #[must_use]
    pub(crate) fn is_erase_confirm_open(&self) -> bool {
        matches!(self.confirm, ConfirmDialog::Erase)
    }

    /// Opens the erase confirmation prompt.
    pub(crate) fn open_erase_confirm(&mut self) {
        self.confirm = ConfirmDialog::Erase;
    }

    /// Closes the erase confirmation prompt.
    pub(crate) fn close_erase_confirm(&mut self) {
        self.confirm = ConfirmDialog::None;
    }

    /// Returns `true` if the quit confirm dialog is open, `false` otherwise.
    #[must_use]
    pub(crate) fn is_quit_confirm_open(&self) -> bool {
        matches!(self.confirm, ConfirmDialog::Quit)
    }

    /// Opens the quit confirmation prompt.
    pub(crate) fn open_quit_confirm(&mut self) {
        self.confirm = ConfirmDialog::Quit;
    }

    /// Closes the quit confirmation prompt.
    pub(crate) fn close_quit_confirm(&mut self) {
        self.confirm = ConfirmDialog::None;
    }

    /// Returns the currently selected ELF path, if any.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the path, or `None` if not set.
    #[must_use]
    pub(crate) fn elf_path(&self) -> Option<&Path> {
        self.elf_path.as_deref()
    }

    /// Sets the ELF path.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the ELF firmware file.
    pub(crate) fn set_elf_path(&mut self, path: PathBuf) {
        self.elf_path = Some(path);
    }

    /// Returns the configured baud rate.
    ///
    /// # Returns
    ///
    /// The serial baud rate in bits per second.
    #[must_use]
    pub(crate) fn baud(&self) -> u32 {
        self.baud
    }

    /// Sets the baud rate.
    ///
    /// # Arguments
    ///
    /// * `baud` - The baud rate in bits per second.
    pub(crate) fn set_baud(&mut self, baud: u32) {
        self.baud = baud;
    }

    /// Opens the ELF path selector popup, optionally pre-filling the input.
    ///
    /// # Arguments
    ///
    /// * `prefill` - If `Some`, the input is pre-populated with this path.
    pub(crate) fn open_elf_selector(&mut self, prefill: Option<&Path>) {
        self.elf_selector = Some(elf::Selector::new(prefill));
    }

    /// Closes the ELF path selector popup.
    pub(crate) fn close_elf_selector(&mut self) {
        self.elf_selector = None;
    }

    /// Returns `true` while the ELF path selector popup is visible.
    ///
    /// # Returns
    ///
    /// `true` if the ELF selector is open, `false` otherwise.
    #[must_use]
    pub(crate) fn is_elf_selector_open(&self) -> bool {
        self.elf_selector.is_some()
    }

    /// Returns a shared reference to the ELF selector, if open.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the [`elf::Selector`], or `None`.
    #[must_use]
    pub(crate) fn elf_selector(&self) -> Option<&elf::Selector> {
        self.elf_selector.as_ref()
    }

    /// Returns the most recent agent telemetry frame, if any has been received.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the latest [`agent_msg::Frame`], or `None`.
    #[must_use]
    pub(crate) fn agent_frame(&self) -> Option<&agent_msg::Frame> {
        self.agent_frame.as_ref()
    }

    /// Returns the agent startup info received at boot, if any.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the [`agent_msg::Startup`], or `None`.
    #[must_use]
    pub(crate) fn agent_startup(&self) -> Option<&agent_msg::Startup> {
        self.agent_startup.as_ref()
    }

    /// Returns the last received partition table, if any.
    ///
    /// # Returns
    ///
    /// A reference to the partition list, or `None` before the first agent
    /// startup message is received.
    #[must_use]
    pub(crate) fn agent_partitions(
        &self,
    ) -> Option<&heapless::Vec<agent_msg::Partition, { agent_msg::MAX_PARTITIONS }>>
    {
        self.agent_partitions.as_ref()
    }

    /// Returns the `Instant` when the last agent message arrived, if any.
    ///
    /// # Returns
    ///
    /// `Some` with the [`Instant`] of the last agent message, or `None`.
    #[must_use]
    pub(crate) fn agent_last_seen(&self) -> Option<Instant> {
        self.agent_last_seen
    }

    /// Returns the `Instant` when the current port connection was established,
    /// if any.
    ///
    /// # Returns
    ///
    /// `Some` with the [`Instant`] of the connection, or `None` when
    /// disconnected.
    #[must_use]
    pub(crate) fn connected_at(&self) -> Option<Instant> {
        self.connected_at
    }

    /// Returns which pane currently has keyboard focus.
    ///
    /// # Returns
    ///
    /// The active [`Pane`].
    #[must_use]
    pub(crate) fn focused_pane(&self) -> Pane {
        self.focused_pane
    }

    /// Returns the Serial Monitor pane width as a percentage of the main area.
    ///
    /// # Returns
    ///
    /// A value in `[0, 100]`; the Inspector pane takes `100 - monitor_pct`.
    #[must_use]
    pub(crate) fn monitor_pct(&self) -> u16 {
        self.monitor_pct
    }

    /// Sets the monitor pane percentage, clamped to `[0, 100]`.
    ///
    /// # Arguments
    ///
    /// * `pct` - Desired width percentage for the Serial Monitor pane.
    pub(crate) fn set_monitor_pct(&mut self, pct: u16) {
        self.monitor_pct = pct.min(100);
    }

    /// Increases the monitor pane width by 5%, clamped to 100%.
    pub(crate) fn grow_monitor(&mut self) {
        self.monitor_pct = self.monitor_pct.saturating_add(5).min(100);
    }

    /// Decreases the monitor pane width by 5%, clamped to 0%.
    pub(crate) fn shrink_monitor(&mut self) {
        self.monitor_pct = self.monitor_pct.saturating_sub(5);
    }

    /// Sets the focused pane directly.
    ///
    /// # Arguments
    ///
    /// * `pane` - The [`Pane`] to focus.
    pub(crate) fn set_focused_pane(&mut self, pane: Pane) {
        self.focused_pane = pane;
    }

    /// Returns a shared reference to the loaded configuration.
    ///
    /// # Returns
    ///
    /// A reference to the active [`config::Config`].
    #[must_use]
    pub(crate) fn config(&self) -> &config::Config {
        &self.config
    }

    fn keys_for_action(
        &self,
        action: MappableAction,
    ) -> Vec<(KeyCode, KeyModifiers)> {
        self.keymap
            .iter()
            .filter(|(_, &a)| a == action)
            .map(|(&k, _)| k)
            .collect()
    }

    /// Returns the display string for the key currently bound to `action`.
    ///
    /// Picks the simplest bound key (plain char over special key over modified
    /// key). Returns `"?"` when no key is bound.
    ///
    /// # Arguments
    ///
    /// * `action` - The action to look up.
    ///
    /// # Returns
    ///
    /// A short string such as `"F"`, `"^F"`, `"↑"`, or `"Tab"`.
    #[must_use]
    pub(crate) fn key_display(&self, action: MappableAction) -> String {
        let keys = self.keys_for_action(action);
        if keys.is_empty() {
            return "?".to_owned();
        }
        let (code, mods) = pick_best_key(&keys);
        format_key_display(code, mods)
    }

    /// Returns a formatted hint string for the key bound to `action`.
    ///
    /// Produces `[F]lash`-style output when the bound key is a plain
    /// character matching the label's first letter, `[C]Flash` when it is a
    /// plain character that does not match, and `[^F] Label` for modifier
    /// combinations.
    ///
    /// # Arguments
    ///
    /// * `action` - The action to look up.
    /// * `label` - The human-readable label for the action.
    ///
    /// # Returns
    ///
    /// A formatted hint string.
    #[must_use]
    pub(crate) fn key_hint(&self, action: MappableAction, label: &str) -> String {
        let keys = self.keys_for_action(action);
        if keys.is_empty() {
            return format!("({label})");
        }
        let (code, mods) = pick_best_key(&keys);
        match (code, mods) {
            (KeyCode::Char(c), m) if m.is_empty() => {
                let c_up = c.to_ascii_uppercase();
                let rest =
                    label.char_indices().nth(1).map_or("", |(i, _)| &label[i..]);
                let label_first = label
                    .chars()
                    .next()
                    .map_or(c_up, |ch| ch.to_ascii_uppercase());
                if c_up == label_first {
                    format!("[{c_up}]{rest}")
                } else {
                    format!("[{c_up}]{label}")
                }
            }
            _ => {
                let k = format_key_display(code, mods);
                format!("[{k}] {label}")
            }
        }
    }

    /// Returns the inspector scroll offset.
    ///
    /// # Returns
    ///
    /// Number of task rows scrolled above the top of the visible inspector area.
    #[must_use]
    pub(crate) fn inspector_scroll(&self) -> usize {
        self.inspector_scroll
    }

    /// Returns the current maximum scroll offset for the inspector pane.
    ///
    /// # Returns
    ///
    /// The value last written by [`Self::set_inspector_max_scroll`], or `0`
    /// before the first render.
    #[must_use]
    pub(crate) fn inspector_max_scroll(&self) -> usize {
        self.inspector_max_scroll.get()
    }

    /// Records the maximum scroll offset for the inspector pane.
    ///
    /// Called by the renderer on every frame with the value
    /// `total_lines.saturating_sub(viewport_height)` so that the scroll-down
    /// key cannot scroll past the last line of content.
    ///
    /// # Arguments
    ///
    /// * `max` - Maximum valid value for `inspector_scroll`.
    pub(crate) fn set_inspector_max_scroll(&self, max: usize) {
        self.inspector_max_scroll.set(max);
    }
}
#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc;

    use super::{build_keymap, pick_best_key};
    use crate::app::{
        format_key_display, Action, App, MappableAction, Pane, DEFAULT_BAUD,
    };
    use crate::config::Config;
    use crate::runner::{
        handle_action, handle_event_message, handle_ports_detected,
    };
    use crate::{flash, log};

    fn app() -> App {
        App::new(None, Config::default())
    }

    fn app_with_port(port: &str) -> App {
        App::new(Some(port.into()), Config::default())
    }

    fn make_tx() -> mpsc::UnboundedSender<crate::event::Message> {
        let (tx, _) = mpsc::unbounded_channel();
        tx
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn unique_temp_path(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{name}-{n}"))
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn push_agent_frame(app: &mut App, task_count: usize) {
        let tasks: String = (0..task_count)
            .map(|i| format!("task{i}:R:1024:{i}"))
            .collect::<Vec<_>>()
            .join(",");
        app.push_line(&format!(
            "V (1000) esp_agent: heap=142000/320000 min=98000 frag=10 \
             iram=0 psram=0 cpu=50 tasks={tasks}"
        ));
        // No renderer runs in unit tests, so seed inspector_max_scroll with the
        // task count so scroll tests can exercise the clamping logic.
        app.set_inspector_max_scroll(task_count);
    }

    #[test]
    fn app_initial_state() {
        let app = app_with_port("COM1");
        assert!(app.is_running());
        assert_eq!(app.port_name(), Some("COM1"));
        assert_eq!(app.scroll(), 0);
        assert!(app.status_msg().is_none());
        assert!(app.port_selector().is_none());
        assert!(!app.is_flashing());
        assert!(app.device_info().is_none());
        assert!(!app.is_erase_confirm_open());
        assert!(app.elf_path().is_none());
    }

    #[test]
    fn app_new_no_port() {
        let app = app();
        assert!(app.port_name().is_none());
    }

    #[test]
    fn app_quit_stops_running() {
        let mut app = app();
        app.quit();
        assert!(!app.is_running());
    }

    #[test]
    fn app_set_status_and_read() {
        let mut app = app();
        app.set_status("hello");
        assert_eq!(app.status_msg(), Some("hello"));
    }

    #[test]
    fn tick_no_status_is_noop() {
        let mut app = app();
        app.tick();
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn tick_recent_status_is_preserved() {
        let mut app = app();
        app.set_status("hello");
        app.tick();
        assert_eq!(app.status_msg(), Some("hello"));
    }

    #[test]
    fn app_set_port_updates_name_and_clears_selector() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into()]);
        assert!(app.port_selector().is_some());
        app.set_port("COM1".into());
        assert_eq!(app.port_name(), Some("COM1"));
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn app_open_port_selector() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        let sel = app.port_selector().unwrap();
        assert_eq!(sel.ports(), &["COM1", "COM2"]);
    }

    #[test]
    fn refresh_port_selector_closes_on_empty() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into()]);
        app.refresh_port_selector(vec![]);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn refresh_port_selector_updates_list_and_clamps_cursor() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        app.port_selector_mut().unwrap().move_cursor(1);
        app.refresh_port_selector(vec!["COM3".into()]);
        let sel = app.port_selector().unwrap();
        assert_eq!(sel.ports(), &["COM3"]);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn refresh_port_selector_no_op_when_closed() {
        let mut app = app();
        app.refresh_port_selector(vec!["COM1".into()]);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn push_line_adds_entry() {
        let mut app = app();
        app.push_line("I (1) wifi: Connected");
        assert_eq!(app.visible_entries(10).len(), 1);
    }

    #[test]
    fn push_line_records_tag() {
        let mut app = app();
        app.push_line("I (1) wifi: Connected");
        assert!(app.filter().known_tags().iter().any(|t| t == "wifi"));
    }

    #[test]
    fn push_line_blank_line_is_ignored() {
        let mut app = app();
        app.push_line("");
        app.push_line("   ");
        assert!(app.visible_entries(10).is_empty());
    }

    #[test]
    fn push_line_raw_line_does_not_record_tag() {
        let mut app = app();
        app.push_line("some raw output");
        assert!(app.filter().known_tags().is_empty());
    }

    #[test]
    fn push_line_scroll_increments_when_scrolled_up() {
        let mut app = app();
        app.push_line("I (1) tag: first");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        app.push_line("I (1) tag: second");
        assert_eq!(app.scroll(), 2);
    }

    #[test]
    fn push_line_scroll_stays_zero_at_bottom() {
        let mut app = app();
        app.push_line("I (1) tag: first");
        app.push_line("I (1) tag: second");
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn heap_history_accumulates_on_agent_frame() {
        let mut app = app();
        assert!(app.heap_history().is_empty());
        push_agent_frame(&mut app, 0);
        assert_eq!(app.heap_history().len(), 1);
        assert_eq!(app.heap_history()[0], 142_000);
    }

    #[test]
    fn heap_history_caps_at_sparkline_len() {
        let mut app = app();
        for _ in 0..=60 {
            push_agent_frame(&mut app, 0);
        }
        assert_eq!(app.heap_history().len(), 60);
    }

    #[test]
    fn cpu_history_accumulates_on_agent_frame() {
        let mut app = app();
        push_agent_frame(&mut app, 0);
        assert_eq!(app.cpu_history()[0].len(), 1);
        assert_eq!(app.cpu_history()[0][0], 50);
    }

    #[test]
    fn clear_agent_data_resets_history() {
        let mut app = app();
        push_agent_frame(&mut app, 0);
        assert!(!app.heap_history().is_empty());
        app.clear_agent_data();
        assert!(app.heap_history().is_empty());
        assert!(app.cpu_history()[0].is_empty());
        assert!(app.cpu_history()[1].is_empty());
    }

    #[test]
    fn push_line_evicts_oldest_when_buffer_full() {
        const BUF: usize = 10_000;
        let mut app = app();
        for i in 0..=BUF {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        let entries = app.visible_entries(BUF + 1);
        assert_eq!(entries.len(), BUF);
        assert_eq!(entries[0].message(), "line 1");
        assert_eq!(entries[BUF - 1].message(), &format!("line {BUF}"));
    }

    #[test]
    fn visible_entries_empty_buffer() {
        let app = app();
        assert!(app.visible_entries(10).is_empty());
    }

    #[test]
    fn visible_entries_fewer_than_height_returns_all() {
        let mut app = app();
        for i in 0..3 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        assert_eq!(app.visible_entries(10).len(), 3);
    }

    #[test]
    fn visible_entries_more_than_height_returns_tail() {
        let mut app = app();
        for i in 0..10 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        let entries = app.visible_entries(5);
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].message(), "line 5");
        assert_eq!(entries[4].message(), "line 9");
    }

    #[test]
    fn visible_entries_scroll_shifts_start() {
        let mut app = app();
        for i in 0..10 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Up));
        let entries = app.visible_entries(5);
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].message(), "line 3");
        assert_eq!(entries[4].message(), "line 7");
    }

    #[test]
    fn visible_entries_filters_by_search_query() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        app.push_line("E (1) i2c: timeout");
        app.push_line("I (1) wifi: disconnected");
        app.filter_mut().toggle_popup();
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        let entries = app.visible_entries(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message(), "timeout");
    }

    #[test]
    fn visible_entries_search_case_insensitive() {
        let mut app = app();
        app.push_line("I (1) tag: HEAP overflow");
        app.push_line("I (1) tag: stack ok");
        app.filter_mut().toggle_popup();
        app.filter_mut().push_search_char('h');
        app.filter_mut().push_search_char('e');
        app.filter_mut().push_search_char('a');
        app.filter_mut().push_search_char('p');
        let entries = app.visible_entries(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message(), "HEAP overflow");
    }

    #[test]
    fn visible_entries_search_matches_tag() {
        let mut app = app();
        app.push_line("I (1) wifi: ok");
        app.push_line("I (1) i2c: ok");
        app.filter_mut().toggle_popup();
        app.filter_mut().push_search_char('w');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('f');
        app.filter_mut().push_search_char('i');
        let entries = app.visible_entries(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tag(), "wifi");
    }

    #[test]
    fn visible_entries_empty_search_returns_all() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        app.push_line("E (1) i2c: timeout");
        assert_eq!(app.visible_entries(10).len(), 2);
    }

    #[test]
    fn visible_entries_respects_hidden_level() {
        let mut app = app();
        app.push_line("E (1) tag: error line");
        app.push_line("I (1) tag: info line");
        app.filter_mut().toggle_at_cursor();
        let entries = app.visible_entries(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message(), "info line");
    }

    #[test]
    fn handle_key_ctrl_c_quits() {
        let mut app = app();
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn handle_key_q_opens_quit_confirm() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::QuitPrompt);
    }

    #[test]
    fn handle_key_q_exits_scroll_mode_when_scrolled() {
        let mut app = app();
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::None);
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_esc_exits_scroll_mode_when_scrolled() {
        let mut app = app();
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_esc_opens_quit_confirm_when_not_scrolled() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::QuitPrompt);
    }

    #[test]
    fn handle_key_q_exits_inspector_scroll_when_inspector_focused() {
        let mut app = app();
        push_agent_frame(&mut app, 3);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.inspector_scroll(), 1);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::None);
        assert_eq!(app.inspector_scroll(), 0);
    }

    #[test]
    fn handle_key_q_does_not_exit_monitor_scroll_when_inspector_focused() {
        let mut app = app();
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::QuitPrompt);
        assert_eq!(app.scroll(), 1);
    }

    #[test]
    fn handle_key_quit_confirm_y_quits() {
        let mut app = app();
        app.open_quit_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Char('y'))), Action::Quit);
    }

    #[test]
    fn handle_key_quit_confirm_n_closes() {
        let mut app = app();
        app.open_quit_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Char('n'))), Action::None);
        assert!(!app.is_quit_confirm_open());
    }

    #[test]
    fn handle_key_quit_confirm_q_closes() {
        let mut app = app();
        app.open_quit_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::None);
        assert!(!app.is_quit_confirm_open());
    }

    #[test]
    fn handle_key_quit_confirm_esc_closes() {
        let mut app = app();
        app.open_quit_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert!(!app.is_quit_confirm_open());
    }

    #[test]
    fn handle_key_ctrl_c_quits_with_quit_confirm_open() {
        let mut app = app();
        app.open_quit_confirm();
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn handle_key_d_disconnects() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('d'))), Action::Disconnect);
    }

    #[test]
    fn disconnect_clears_port_state() {
        let mut app = app_with_port("COM1");
        app.disconnect();
        assert!(app.port_name().is_none());
        assert!(app.port_cmd_tx().is_none());
    }

    #[test]
    fn handle_key_r_resets_device() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('r'))), Action::ResetDevice);
    }

    #[test]
    fn handle_key_c_scans_ports() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('c'))), Action::ScanPorts);
    }

    #[test]
    fn handle_key_f_returns_flash_action() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('f'))), Action::Flash);
    }

    #[test]
    fn handle_key_e_returns_erase_prompt_action() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('e'))), Action::ErasePrompt);
    }

    #[test]
    fn handle_key_s_is_noop() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Char('s'))), Action::None);
    }

    #[test]
    fn handle_key_tab_cycles_pane_focus() {
        let mut app = app();
        assert_eq!(app.focused_pane(), Pane::Monitor);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Monitor);
    }

    #[test]
    fn handle_key_ctrl_f_toggles_filter_popup_when_monitor_focused() {
        let mut app = app();
        assert!(!app.filter().is_popup_open());
        app.handle_key(ctrl(KeyCode::Char('f')));
        assert!(app.filter().is_popup_open());
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.filter().is_popup_open());
    }

    #[test]
    fn handle_key_ctrl_f_no_op_when_inspector_focused() {
        let mut app = app();
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(ctrl(KeyCode::Char('f')));
        assert!(!app.filter().is_popup_open());
    }

    #[test]
    fn handle_key_up_scrolls_up() {
        let mut app = app();
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
    }

    #[test]
    fn handle_key_down_scrolls_down_and_clamps() {
        let mut app = app();
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.scroll(), 0);
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_page_up_adds_ten() {
        let mut app = app();
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll(), 10);
    }

    #[test]
    fn handle_key_page_down_subtracts_ten() {
        let mut app = app();
        app.handle_key(key(KeyCode::PageUp));
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_unknown_returns_none() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::F(1))), Action::None);
    }

    #[test]
    fn handle_key_filter_popup_space_toggles_item() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        assert!(!app.filter().is_level_hidden(log::Level::Error));
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.filter().is_level_hidden(log::Level::Error));
    }

    #[test]
    fn handle_key_filter_popup_ctrl_a_toggles_all() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(ctrl(KeyCode::Char('a')));
        assert!(app.filter().is_level_hidden(log::Level::Error));
        assert!(app.filter().is_level_hidden(log::Level::Info));
    }

    #[test]
    fn handle_key_filter_popup_q_focuses_search() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.filter().is_popup_open());
        assert!(app.filter().is_search_focused());
        assert_eq!(app.filter().search_query(), "q");
    }

    #[test]
    fn handle_key_filter_popup_esc_closes() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.filter().is_popup_open());
    }

    #[test]
    fn handle_key_filter_popup_char_focuses_search_and_types() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('w')));
        assert!(app.filter().is_search_focused());
        assert_eq!(app.filter().search_query(), "w");
    }

    #[test]
    fn handle_key_filter_popup_space_types_when_focused() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('w')));
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.filter().search_query(), "w ");
        assert!(!app.filter().is_level_hidden(log::Level::Error));
    }

    #[test]
    fn handle_key_filter_popup_backspace_refocuses_search() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('w')));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.filter().is_search_focused());
        app.handle_key(key(KeyCode::Backspace));
        assert!(app.filter().is_search_focused());
        assert_eq!(app.filter().search_query(), "");
    }

    #[test]
    fn handle_key_filter_popup_esc_unfocuses_search_keeping_query() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('w')));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.filter().is_popup_open());
        assert!(!app.filter().is_search_focused());
        assert_eq!(app.filter().search_query(), "w");
    }

    #[test]
    fn handle_key_filter_popup_esc_closes_when_unfocused() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('w')));
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.filter().is_popup_open());
    }

    #[test]
    fn handle_key_filter_popup_up_down_unfocuses_search() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('w')));
        assert!(app.filter().is_search_focused());
        app.handle_key(key(KeyCode::Down));
        assert!(!app.filter().is_search_focused());
    }

    #[test]
    fn handle_key_filter_popup_up_at_top_focuses_search() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        assert_eq!(app.filter().cursor(), 0);
        assert!(!app.filter().is_search_focused());
        app.handle_key(key(KeyCode::Up));
        assert!(app.filter().is_search_focused());
    }

    #[test]
    fn handle_key_filter_popup_up_not_at_top_moves_cursor() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.filter().cursor(), 1);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.filter().cursor(), 0);
        assert!(!app.filter().is_search_focused());
    }

    #[test]
    fn handle_key_filter_popup_ctrl_a_still_toggles_all_when_unfocused() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(ctrl(KeyCode::Char('a')));
        assert!(app.filter().is_level_hidden(log::Level::Error));
        assert!(!app.filter().is_search_focused());
    }

    #[test]
    fn handle_key_filter_popup_navigation() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.filter().cursor(), 1);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.filter().cursor(), 0);
    }

    #[test]
    fn handle_key_ctrl_c_quits_even_with_popup_open() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn handle_key_port_selector_navigation() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.port_selector().unwrap().cursor(), 1);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.port_selector().unwrap().cursor(), 0);
    }

    #[test]
    fn handle_key_port_selector_enter_returns_connect_action() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Action::ConnectPort("COM1".to_owned()));
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_port_selector_c_dismisses() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into()]);
        let action = app.handle_key(key(KeyCode::Char('c')));
        assert_eq!(action, Action::None);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_port_selector_q_dismisses() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into()]);
        let action = app.handle_key(key(KeyCode::Char('q')));
        assert_eq!(action, Action::None);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_port_selector_esc_dismisses() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into()]);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_ctrl_c_quits_even_with_selector_open() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into()]);
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn push_line_scroll_no_drift_when_entry_filtered() {
        let mut app = app();
        app.push_line("E (1) tag: error");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        app.filter_mut().move_cursor(2);
        app.filter_mut().toggle_at_cursor();
        app.push_line("I (1) tag: info filtered");
        assert_eq!(app.scroll(), 1);
        app.push_line("E (1) tag: error visible");
        assert_eq!(app.scroll(), 2);
    }

    #[test]
    fn clear_log_empties_buffer_and_resets_scroll() {
        let mut app = app();
        for i in 0..5 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        app.clear_log();
        assert!(app.visible_entries(10).is_empty());
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_ctrl_l_clears_log_when_monitor_focused() {
        let mut app = app();
        app.push_line("I (1) tag: line");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('l'))), Action::None);
        assert!(app.visible_entries(10).is_empty());
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_ctrl_l_no_op_when_inspector_focused() {
        let mut app = app();
        app.push_line("I (1) tag: line");
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(ctrl(KeyCode::Char('l')));
        assert!(!app.visible_entries(10).is_empty());
    }

    #[test]
    fn handle_ports_detected_no_op_when_empty_and_disconnected() {
        let mut app = app();
        handle_ports_detected(&mut app, vec![], &[], &make_tx());
        assert!(app.port_name().is_none());
        assert!(app.port_selector().is_none());
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn handle_ports_detected_opens_selector_for_multiple_ports() {
        let mut app = app();
        handle_ports_detected(
            &mut app,
            vec!["COM1".into(), "COM2".into()],
            &[],
            &make_tx(),
        );
        assert!(app.port_selector().is_some());
    }

    #[test]
    fn handle_ports_detected_refreshes_open_selector() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        handle_ports_detected(
            &mut app,
            vec!["COM3".into(), "COM4".into()],
            &["COM1".to_owned(), "COM2".to_owned()],
            &make_tx(),
        );
        let sel = app.port_selector().unwrap();
        assert_eq!(sel.ports(), &["COM3", "COM4"]);
    }

    #[test]
    fn handle_ports_detected_closes_selector_on_empty() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into()]);
        handle_ports_detected(&mut app, vec![], &["COM1".to_owned()], &make_tx());
        assert!(app.port_selector().is_none());
        assert_eq!(app.status_msg(), Some("No devices detected."));
    }

    #[tokio::test]
    async fn handle_ports_detected_auto_connects_when_selector_reaches_one_port() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        handle_ports_detected(
            &mut app,
            vec!["COM1".into()],
            &["COM1".to_owned(), "COM2".to_owned()],
            &make_tx(),
        );
        assert!(
            app.port_selector().is_none(),
            "selector must close when reduced to one port"
        );
    }

    #[test]
    fn handle_ports_detected_connected_new_device_sets_status() {
        let mut app = app();
        app.set_port("COM1".into());
        handle_ports_detected(
            &mut app,
            vec!["COM1".into(), "COM2".into()],
            &["COM1".to_owned()],
            &make_tx(),
        );
        assert!(app.status_msg().is_some());
    }

    #[test]
    fn handle_ports_detected_connected_same_ports_no_status() {
        let mut app = app();
        app.set_port("COM1".into());
        handle_ports_detected(
            &mut app,
            vec!["COM1".into()],
            &["COM1".to_owned()],
            &make_tx(),
        );
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn handle_ports_detected_connected_current_gone_no_new_device_status() {
        let mut app = app();
        app.set_port("COM1".into());
        handle_ports_detected(
            &mut app,
            vec!["COM2".into()],
            &["COM1".to_owned(), "COM2".to_owned()],
            &make_tx(),
        );
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn handle_ports_detected_other_port_disappeared_no_status() {
        let mut app = app();
        app.set_port("COM1".into());
        handle_ports_detected(
            &mut app,
            vec!["COM1".into()],
            &["COM1".to_owned(), "COM2".to_owned()],
            &make_tx(),
        );
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn handle_ports_detected_new_device_but_connected_port_gone_no_status() {
        let mut app = app();
        app.set_port("COM1".into());
        handle_ports_detected(
            &mut app,
            vec!["COM2".into(), "COM3".into()],
            &["COM1".to_owned()],
            &make_tx(),
        );
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn handle_ports_detected_is_noop_while_flashing() {
        let mut app = app();
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_ports_detected(
            &mut app,
            vec!["/dev/ttyUSB0".into()],
            &[],
            &make_tx(),
        );
        assert!(app.port_selector().is_none());
        assert!(app.port_name().is_none());
    }

    #[test]
    fn handle_action_quit() {
        let mut app = app();
        handle_action(&mut app, Action::Quit, &make_tx());
        assert!(!app.is_running());
    }

    #[test]
    fn handle_action_quit_while_flashing_quits_immediately() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::Quit, &make_tx());
        assert!(!app.is_running());
    }

    #[test]
    fn handle_action_quit_prompt_while_flashing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::QuitPrompt, &make_tx());
        assert!(!app.is_quit_confirm_open());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn handle_action_quit_prompt_while_erasing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Erasing);
        handle_action(&mut app, Action::QuitPrompt, &make_tx());
        assert!(!app.is_quit_confirm_open());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn scan_ports_while_flashing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::ScanPorts, &make_tx());
        assert!(app.port_selector().is_none());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn handle_action_disconnect_when_connected() {
        let mut app = app_with_port("COM1");
        handle_action(&mut app, Action::Disconnect, &make_tx());
        assert!(app.port_name().is_none());
        assert_eq!(app.status_msg(), Some("Disconnected."));
    }

    #[test]
    fn handle_action_disconnect_when_not_connected() {
        let mut app = app();
        handle_action(&mut app, Action::Disconnect, &make_tx());
        assert_eq!(app.status_msg(), Some("Not connected."));
    }

    #[test]
    fn handle_action_disconnect_while_flashing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::Disconnect, &make_tx());
        assert!(app.port_name().is_some());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn handle_action_connect_port_while_flashing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::ConnectPort("COM2".into()), &make_tx());
        assert_eq!(app.port_name(), Some("COM1"));
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn handle_action_reset_no_port() {
        let mut app = app();
        handle_action(&mut app, Action::ResetDevice, &make_tx());
        assert_eq!(app.status_msg(), Some("No port connected."));
    }

    #[test]
    fn handle_action_reset_while_flashing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::ResetDevice, &make_tx());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn handle_action_reset_while_erasing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Erasing);
        handle_action(&mut app, Action::ResetDevice, &make_tx());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn handle_action_none_is_noop() {
        let mut app = app();
        handle_action(&mut app, Action::None, &make_tx());
        assert!(app.status_msg().is_none());
        assert!(app.is_running());
    }

    #[test]
    fn connect_success_commits_new_port_and_kills_old_source() {
        let (old_src_tx, _old_src_rx) = tokio::sync::watch::channel(false);
        let (new_src_tx, _new_src_rx) = tokio::sync::watch::channel(false);
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel();

        let mut app = app_with_port("COM1");
        app.set_source_shutdown(old_src_tx);

        app.set_port("COM2".into());
        app.set_port_cmd(cmd_tx);
        app.set_source_shutdown(new_src_tx);
        app.set_status("Connected to COM2.");

        assert_eq!(app.port_name(), Some("COM2"));
        assert_eq!(app.status_msg(), Some("Connected to COM2."));
    }

    #[test]
    fn connect_success_while_reconnecting_clears_flash_state() {
        let (src_tx, _src_rx) = tokio::sync::watch::channel(false);
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel();
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Reconnecting);
        assert!(app.is_flashing());
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::ConnectSuccess {
                port: "COM1".into(),
                cmd_tx,
                src_tx,
            },
            DEFAULT_BAUD,
            &tx,
        );
        assert!(
            !app.is_flashing(),
            "ConnectSuccess must clear Reconnecting state"
        );
        assert_eq!(app.port_name(), Some("COM1"));
    }

    #[test]
    fn connect_error_clears_port_and_sets_status() {
        let mut app = app_with_port("COM1");
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::ConnectError("failed: resource busy".into()),
            DEFAULT_BAUD,
            &tx,
        );
        assert!(
            app.port_name().is_none(),
            "ConnectError must clear port_name via disconnect"
        );
        assert_eq!(app.status_msg(), Some("failed: resource busy"));
        assert!(!app.is_flashing(), "Reconnecting state must be cleared");
    }

    #[test]
    fn handle_action_scan_ports_leaves_app_in_consistent_state() {
        let mut app = app();
        handle_action(&mut app, Action::ScanPorts, &make_tx());
        assert!(
            app.status_msg().is_some()
                || app.port_name().is_some()
                || app.port_selector().is_some(),
            "scan_ports must produce an observable state change"
        );
    }

    #[test]
    fn handle_key_erase_confirm_y_confirms() {
        let mut app = app();
        app.open_erase_confirm();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('y'))),
            Action::ConfirmErase
        );
    }

    #[test]
    fn handle_key_erase_confirm_n_closes() {
        let mut app = app();
        app.open_erase_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Char('n'))), Action::None);
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_key_erase_confirm_esc_closes() {
        let mut app = app();
        app.open_erase_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_key_erase_confirm_e_closes() {
        let mut app = app();
        app.open_erase_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Char('e'))), Action::None);
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_key_ctrl_c_quits_with_erase_confirm_open() {
        let mut app = app();
        app.open_erase_confirm();
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn handle_key_elf_selector_char_updates_input() {
        let mut app = app();
        app.open_elf_selector(None);
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.elf_selector().unwrap().value(), "/t");
    }

    #[test]
    fn handle_key_elf_selector_esc_closes() {
        let mut app = app();
        app.open_elf_selector(None);
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::CloseElfSelector);
    }

    #[test]
    fn handle_key_elf_selector_enter_confirms() {
        let mut app = app();
        app.open_elf_selector(None);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::ConfirmElfPath);
    }

    #[test]
    fn handle_key_elf_selector_enter_while_cycling_accepts_not_confirms() {
        let dir = std::env::temp_dir().join(format!(
            "esp-tui-app-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fw_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        std::fs::write(dir.join("fw_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();

        let mut app = app();
        app.open_elf_selector(None);
        for ch in format!("{}/fw", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::ConfirmElfPath);
    }

    #[test]
    fn handle_key_elf_selector_back_tab_noop_when_no_completions() {
        let mut app = app();
        app.open_elf_selector(None);
        assert_eq!(app.handle_key(key(KeyCode::BackTab)), Action::None);
    }

    #[test]
    fn handle_action_flash_always_opens_selector() {
        let mut app = app_with_port("COM1");
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(app.is_elf_selector_open());
    }

    #[test]
    fn handle_action_confirm_elf_path_no_port_sets_status() {
        let path = unique_temp_path("esp-tui-test-elf-no-port");
        std::fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app();
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector_mut() {
            for ch in path.to_str().unwrap().chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::ConfirmElfPath, &make_tx());
        assert_eq!(app.status_msg(), Some("No port connected."));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handle_action_confirm_elf_path_already_flashing_sets_status() {
        let path = unique_temp_path("esp-tui-test-elf-flashing");
        std::fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector_mut() {
            for ch in path.to_str().unwrap().chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::ConfirmElfPath, &make_tx());
        assert_eq!(app.status_msg(), Some("Flash already in progress."));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handle_action_confirm_elf_path_valid() {
        let path = unique_temp_path("esp-tui-test-elf");
        std::fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app();
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector_mut() {
            for ch in path.to_str().unwrap().chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::ConfirmElfPath, &make_tx());
        assert_eq!(app.elf_path(), Some(path.as_path()));
        assert!(!app.is_elf_selector_open());
        assert_eq!(app.status_msg(), Some("No port connected."));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handle_action_confirm_elf_path_nonexistent_stays_open() {
        let mut app = app();
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector_mut() {
            for ch in "/nonexistent/path.elf".chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::ConfirmElfPath, &make_tx());
        assert!(app.elf_path().is_none());
        assert!(app.is_elf_selector_open());
        assert_eq!(app.status_msg(), Some("Path not found."));
    }

    #[test]
    fn handle_action_confirm_elf_path_directory_rejected() {
        let dir = std::env::temp_dir();
        let mut app = app();
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector_mut() {
            for ch in dir.to_str().unwrap().chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::ConfirmElfPath, &make_tx());
        assert!(app.elf_path().is_none());
        assert!(app.is_elf_selector_open());
        assert_eq!(app.status_msg(), Some("Path is a directory."));
    }

    #[test]
    fn handle_action_confirm_elf_path_non_elf_rejected() {
        let path = unique_temp_path("esp-tui-test-non-elf");
        std::fs::write(&path, b"not an elf file").unwrap();
        let mut app = app();
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector_mut() {
            for ch in path.to_str().unwrap().chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::ConfirmElfPath, &make_tx());
        assert!(app.elf_path().is_none());
        assert!(app.is_elf_selector_open());
        assert_eq!(app.status_msg(), Some("Not a valid ELF file."));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn is_flashing_reflects_state() {
        let mut app = app();
        assert!(!app.is_flashing());
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 100,
        });
        assert!(app.is_flashing());
        app.set_flash_state(crate::flash::State::Erasing);
        assert!(app.is_flashing());
        app.set_flash_state(crate::flash::State::Reconnecting);
        assert!(app.is_flashing());
        app.set_flash_state(crate::flash::State::Idle);
        assert!(!app.is_flashing());
    }

    #[test]
    fn handle_action_erase_prompt_no_port_sets_status() {
        let mut app = app();
        handle_action(&mut app, Action::ErasePrompt, &make_tx());
        assert_eq!(app.status_msg(), Some("No port connected."));
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_action_erase_prompt_while_flashing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::ErasePrompt, &make_tx());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_action_erase_prompt_connected_opens_confirm() {
        let mut app = app_with_port("COM1");
        handle_action(&mut app, Action::ErasePrompt, &make_tx());
        assert!(app.is_erase_confirm_open());
    }

    #[test]
    fn handle_action_flash_no_port_sets_status_and_does_not_open_selector() {
        let mut app = app();
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(!app.is_elf_selector_open());
        assert_eq!(app.status_msg(), Some("No port connected."));
    }

    #[test]
    fn handle_action_flash_opens_selector_prefilled_when_elf_set() {
        let mut app = app_with_port("COM1");
        app.set_elf_path(std::path::PathBuf::from("/tmp/firmware.elf"));
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(app.is_elf_selector_open());
        assert_eq!(app.elf_selector().unwrap().value(), "/tmp/firmware.elf");
    }

    #[test]
    fn handle_action_close_elf_selector_closes() {
        let mut app = app();
        app.open_elf_selector(None);
        handle_action(&mut app, Action::CloseElfSelector, &make_tx());
        assert!(!app.is_elf_selector_open());
    }

    #[test]
    fn handle_action_flash_while_flashing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(!app.is_elf_selector_open());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn handle_action_flash_while_erasing_sets_status() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Erasing);
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(!app.is_elf_selector_open());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
    }

    #[test]
    fn flash_done_ok_sets_reconnecting_and_status() {
        let mut app = app();
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::FlashDone(Ok(())),
            DEFAULT_BAUD,
            &tx,
        );
        assert!(matches!(app.flash_state(), flash::State::Reconnecting));
        assert_eq!(app.status_msg(), Some("Flash complete. Reconnecting..."));
    }

    #[test]
    fn flash_done_err_sets_reconnecting_and_error_status() {
        let mut app = app();
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::FlashDone(Err(anyhow::anyhow!("write error"))),
            DEFAULT_BAUD,
            &tx,
        );
        assert!(matches!(app.flash_state(), flash::State::Reconnecting));
        assert!(app.status_msg().unwrap_or("").contains("Flash failed"));
    }

    #[test]
    fn erase_done_ok_sets_reconnecting() {
        let mut app = app();
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::EraseDone(Ok(())),
            DEFAULT_BAUD,
            &tx,
        );
        assert!(matches!(app.flash_state(), flash::State::Reconnecting));
        assert_eq!(app.status_msg(), Some("Erase complete."));
    }

    #[test]
    fn erase_done_err_sets_reconnecting_and_status() {
        let mut app = app();
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::EraseDone(Err(anyhow::anyhow!("erase error"))),
            DEFAULT_BAUD,
            &tx,
        );
        assert!(matches!(app.flash_state(), flash::State::Reconnecting));
        assert!(app.status_msg().unwrap_or("").contains("Erase failed"));
    }

    #[test]
    fn device_info_ok_stores_info() {
        let mut app = app();
        let tx = make_tx();
        let info = flash::DeviceInfo::new("ESP32-S3", "4MB", "AA:BB:CC:DD:EE:FF");
        handle_event_message(
            &mut app,
            crate::event::Message::DeviceInfo(Ok(info)),
            DEFAULT_BAUD,
            &tx,
        );
        assert!(app.device_info().is_some());
    }

    #[test]
    fn device_info_err_is_ignored() {
        let mut app = app();
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::DeviceInfo(Err(anyhow::anyhow!("probe failed"))),
            DEFAULT_BAUD,
            &tx,
        );
        assert!(app.device_info().is_none());
    }

    #[test]
    fn scroll_routes_to_monitor_when_focused() {
        let mut app = app();
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        assert_eq!(app.inspector_scroll(), 0);
    }

    #[test]
    fn scroll_routes_to_inspector_when_focused() {
        let mut app = app();
        push_agent_frame(&mut app, 3);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.inspector_scroll(), 1);
        assert_eq!(app.scroll(), 0);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.inspector_scroll(), 0);
    }

    #[test]
    fn page_scroll_routes_to_inspector_when_focused() {
        let mut app = app();
        push_agent_frame(&mut app, 12);
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.inspector_scroll(), 10);
        assert_eq!(app.scroll(), 0);
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.inspector_scroll(), 0);
    }

    #[test]
    fn push_line_agent_frame_populated() {
        let mut app = app();
        assert!(app.agent_frame().is_none());
        push_agent_frame(&mut app, 0);
        assert!(app.agent_frame().is_some());
    }

    #[test]
    fn push_line_agent_startup_populated() {
        let mut app = app();
        assert!(app.agent_startup().is_none());
        app.push_line(
            "V (100) esp_agent: start reason=poweron chip=esp32s3 \
             cores=2 rev=1 mac=AA:BB:CC:DD:EE:FF flash=0x400000",
        );
        assert!(app.agent_startup().is_some());
    }

    #[test]
    fn push_line_agent_last_seen_set() {
        let mut app = app();
        assert!(app.agent_last_seen().is_none());
        push_agent_frame(&mut app, 0);
        assert!(app.agent_last_seen().is_some());
    }

    #[test]
    fn disconnect_clears_agent_data_and_connected_at() {
        let mut app = app_with_port("COM1");
        push_agent_frame(&mut app, 0);
        assert!(app.agent_last_seen().is_some());
        app.disconnect();
        assert!(app.agent_last_seen().is_none());
        assert!(app.agent_frame().is_none());
        assert!(app.connected_at().is_none());
    }

    #[test]
    fn set_port_records_connected_at() {
        let mut app = app();
        assert!(app.connected_at().is_none());
        app.set_port("COM1".into());
        assert!(app.connected_at().is_some());
    }

    #[test]
    fn monitor_pct_initial_value() {
        let app = app();
        assert_eq!(app.monitor_pct(), 60);
    }

    #[test]
    fn ctrl_right_grows_monitor_when_focused() {
        let mut app = app();
        assert_eq!(app.focused_pane(), Pane::Monitor);
        app.handle_key(ctrl(KeyCode::Right));
        assert_eq!(app.monitor_pct(), 65);
    }

    #[test]
    fn ctrl_left_shrinks_monitor_when_focused() {
        let mut app = app();
        assert_eq!(app.focused_pane(), Pane::Monitor);
        app.handle_key(ctrl(KeyCode::Left));
        assert_eq!(app.monitor_pct(), 55);
    }

    #[test]
    fn ctrl_right_with_inspector_focused_grows_monitor() {
        let mut app = app();
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(ctrl(KeyCode::Right));
        assert_eq!(app.monitor_pct(), 65);
    }

    #[test]
    fn ctrl_left_with_inspector_focused_shrinks_monitor() {
        let mut app = app();
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(ctrl(KeyCode::Left));
        assert_eq!(app.monitor_pct(), 55);
    }

    #[test]
    fn resize_clamps_at_100() {
        let mut app = app();
        for _ in 0..9 {
            app.handle_key(ctrl(KeyCode::Right));
        }
        assert_eq!(app.monitor_pct(), 100);
    }

    #[test]
    fn resize_clamps_at_0() {
        let mut app = app();
        for _ in 0..13 {
            app.handle_key(ctrl(KeyCode::Left));
        }
        assert_eq!(app.monitor_pct(), 0);
        assert_eq!(app.focused_pane(), Pane::Inspector);
    }

    #[test]
    fn ctrl_left_on_monitor_auto_cycles_to_inspector_at_zero() {
        let mut app = app();
        for _ in 0..12 {
            app.handle_key(ctrl(KeyCode::Left));
        }
        assert_eq!(app.monitor_pct(), 0);
        assert_eq!(app.focused_pane(), Pane::Inspector);
    }

    #[test]
    fn ctrl_right_on_inspector_auto_cycles_to_monitor_at_hundred() {
        let mut app = app();
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        for _ in 0..8 {
            app.handle_key(ctrl(KeyCode::Right));
        }
        assert_eq!(app.monitor_pct(), 100);
        assert_eq!(app.focused_pane(), Pane::Monitor);
    }

    #[test]
    fn tab_auto_expands_collapsed_inspector() {
        let mut app = app();
        for _ in 0..8 {
            app.handle_key(ctrl(KeyCode::Right));
        }
        assert_eq!(app.monitor_pct(), 100);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        assert_eq!(app.monitor_pct(), 80);
    }

    #[test]
    fn tab_auto_expands_collapsed_monitor() {
        let mut app = app();
        for _ in 0..12 {
            app.handle_key(ctrl(KeyCode::Left));
        }
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focused_pane(), Pane::Monitor);
        assert_eq!(app.monitor_pct(), 20);
    }

    #[test]
    fn format_key_display_plain_char() {
        assert_eq!(
            format_key_display(KeyCode::Char('j'), KeyModifiers::empty()),
            "J"
        );
    }

    #[test]
    fn format_key_display_ctrl() {
        assert_eq!(
            format_key_display(KeyCode::Char('f'), KeyModifiers::CONTROL),
            "^F"
        );
    }

    #[test]
    fn format_key_display_special_keys() {
        assert_eq!(format_key_display(KeyCode::Up, KeyModifiers::empty()), "↑");
        assert_eq!(
            format_key_display(KeyCode::PageUp, KeyModifiers::empty()),
            "PgUp"
        );
        assert_eq!(
            format_key_display(KeyCode::Tab, KeyModifiers::empty()),
            "Tab"
        );
        assert_eq!(
            format_key_display(KeyCode::F(5), KeyModifiers::empty()),
            "F5"
        );
    }

    #[test]
    fn pick_best_key_prefers_plain_char() {
        let keys = vec![
            (KeyCode::Up, KeyModifiers::empty()),
            (KeyCode::Char('k'), KeyModifiers::empty()),
            (KeyCode::Char('k'), KeyModifiers::CONTROL),
        ];
        let (code, mods) = pick_best_key(&keys);
        assert_eq!(code, KeyCode::Char('k'));
        assert_eq!(mods, KeyModifiers::empty());
    }

    #[test]
    fn pick_best_key_prefers_unmodified_special_over_modified() {
        let keys = vec![
            (KeyCode::Up, KeyModifiers::CONTROL),
            (KeyCode::Up, KeyModifiers::empty()),
        ];
        let (code, mods) = pick_best_key(&keys);
        assert_eq!(code, KeyCode::Up);
        assert_eq!(mods, KeyModifiers::empty());
    }

    #[test]
    fn build_keymap_vim_preset_maps_slash_to_toggle_filter() {
        use crate::config::KeysConfig;
        let cfg = KeysConfig {
            preset: Some("vim".to_owned()),
            overrides: std::collections::HashMap::new(),
        };
        let map = build_keymap(&cfg);
        assert_eq!(
            map.get(&(KeyCode::Char('/'), KeyModifiers::empty())),
            Some(&MappableAction::ToggleFilter),
            "'/' should map to toggle_filter in vim preset"
        );
    }

    #[test]
    fn build_keymap_vim_preset_maps_j_k() {
        use crate::config::KeysConfig;
        let cfg = KeysConfig {
            preset: Some("vim".to_owned()),
            overrides: std::collections::HashMap::new(),
        };
        let map = build_keymap(&cfg);
        assert_eq!(
            map.get(&(KeyCode::Char('j'), KeyModifiers::empty())),
            Some(&MappableAction::ScrollDown)
        );
        assert_eq!(
            map.get(&(KeyCode::Char('k'), KeyModifiers::empty())),
            Some(&MappableAction::ScrollUp)
        );
    }

    #[test]
    fn build_keymap_override_replaces_default_binding() {
        use crate::config::KeysConfig;
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("x".to_owned(), "quit_prompt".to_owned());
        let cfg = KeysConfig {
            preset: None,
            overrides,
        };
        let map = build_keymap(&cfg);
        assert_eq!(
            map.get(&(KeyCode::Char('x'), KeyModifiers::empty())),
            Some(&MappableAction::QuitPrompt)
        );
        assert!(
            !map.contains_key(&(KeyCode::Char('q'), KeyModifiers::empty())),
            "old 'q' binding should have been removed"
        );
        assert!(
            !map.contains_key(&(KeyCode::Esc, KeyModifiers::empty())),
            "old 'Esc' binding should have been removed"
        );
    }

    #[test]
    fn build_keymap_override_on_top_of_preset() {
        use crate::config::KeysConfig;
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("n".to_owned(), "scroll_down".to_owned());
        let cfg = KeysConfig {
            preset: Some("vim".to_owned()),
            overrides,
        };
        let map = build_keymap(&cfg);
        assert_eq!(
            map.get(&(KeyCode::Char('n'), KeyModifiers::empty())),
            Some(&MappableAction::ScrollDown)
        );
        assert!(
            !map.contains_key(&(KeyCode::Char('j'), KeyModifiers::empty())),
            "preset 'j' binding replaced by override"
        );
    }

    #[test]
    fn build_keymap_vim_preset_maps_uppercase_g_with_shift() {
        use crate::config::KeysConfig;
        let cfg = KeysConfig {
            preset: Some("vim".to_owned()),
            overrides: std::collections::HashMap::new(),
        };
        let map = build_keymap(&cfg);
        assert_eq!(
            map.get(&(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(&MappableAction::ScrollBottom),
            "'G' must be stored with SHIFT so crossterm's Shift+G event matches"
        );
    }

    fn app_with_vim_preset() -> App {
        use crate::config::{Config, KeysConfig};
        let cfg = Config {
            keys: KeysConfig {
                preset: Some("vim".to_owned()),
                overrides: std::collections::HashMap::default(),
            },
            ..Config::default()
        };
        App::new(None, cfg)
    }

    #[test]
    fn scroll_top_shows_oldest_entries() {
        let mut app = app_with_vim_preset();
        for i in 0..20 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        app.handle_key(key(KeyCode::Char('g')));
        let entries = app.visible_entries(5);
        assert_eq!(entries[0].message(), "line 0", "g should show oldest first");
    }

    #[test]
    fn scroll_bottom_shows_newest_entries() {
        let mut app = app_with_vim_preset();
        for i in 0..20 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        app.handle_key(key(KeyCode::Char('g')));
        app.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        let entries = app.visible_entries(5);
        assert_eq!(entries[4].message(), "line 19", "G should show newest last");
    }

    #[test]
    fn scroll_top_in_inspector_does_not_move_monitor_scroll() {
        let mut app = app_with_vim_preset();
        for i in 0..20 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.scroll(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.focused_pane(), Pane::Inspector);
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(
            app.scroll(),
            1,
            "monitor scroll must not change when inspector is focused"
        );
        assert_eq!(app.inspector_scroll(), 0);
    }

    #[test]
    fn slash_closes_filter_popup_with_vim_preset() {
        let mut app = app_with_vim_preset();
        app.handle_key(key(KeyCode::Char('/')));
        assert!(
            app.filter().is_popup_open(),
            "/ should open the filter popup"
        );
        app.handle_key(key(KeyCode::Char('/')));
        assert!(
            !app.filter().is_popup_open(),
            "/ should close the filter popup"
        );
    }
}

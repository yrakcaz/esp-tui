use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::watch;

use esp_agent_msg as agent_msg;

use crate::{backtrace, config, elf, filter, flash, log, port, serial};

pub(crate) const DEFAULT_BAUD: u32 = 115_200;
const STATUS_TTL_SECS: u64 = 3;
// ESP-IDF prints "Guru Meditation Error: ..." a handful of lines before the
// "Backtrace:" line; this bounds how far back to look for it as a header.
const GURU_MEDITATION_LOOKBACK: usize = 20;
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
    SearchNext,
    SearchPrev,
    Dismiss,
    ToggleBacktrace,
}

pub(crate) type KeyMap = HashMap<(KeyCode, KeyModifiers), MappableAction>;

pub(crate) fn default_keymap() -> KeyMap {
    let none = KeyModifiers::empty();
    let ctrl = KeyModifiers::CONTROL;
    let shift = KeyModifiers::SHIFT;
    [
        ((KeyCode::Char('q'), none), MappableAction::QuitPrompt),
        ((KeyCode::Esc, none), MappableAction::Dismiss),
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
        ((KeyCode::Char('n'), none), MappableAction::SearchNext),
        ((KeyCode::Char('N'), shift), MappableAction::SearchPrev),
        ((KeyCode::Char('b'), none), MappableAction::ToggleBacktrace),
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
        KeyCode::Char(c) => {
            if ctrl || alt {
                format!("{}{}", prefix, c.to_ascii_uppercase())
            } else {
                c.to_string()
            }
        }
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
        "search_next" => Some(MappableAction::SearchNext),
        "search_prev" => Some(MappableAction::SearchPrev),
        "dismiss" => Some(MappableAction::Dismiss),
        "toggle_backtrace" => Some(MappableAction::ToggleBacktrace),
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
    /// Confirm the ELF path currently typed in the backtrace popup's input,
    /// re-resolving the displayed report without triggering a flash.
    LoadBacktraceElf,
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

/// Dispatches Tab/BackTab/Enter/typing to a text-input-with-autocomplete
/// widget, shared by the flash ELF selector and the backtrace popup's ELF
/// box (both wrap an [`elf::Selector`] and only differ in what Enter should
/// do once there's no completion menu left to accept).
///
/// # Arguments
///
/// * `selector` - The active selector, if the popup owning it is open.
/// * `key` - The key event to handle.
/// * `on_confirm` - The [`Action`] to return when Enter is pressed with no
///   completion menu open.
///
/// # Returns
///
/// [`Action::None`] for every key except a confirming Enter, which returns
/// `on_confirm`.
fn handle_elf_input_key(
    selector: Option<&mut elf::Selector>,
    key: KeyEvent,
    on_confirm: Action,
) -> Action {
    selector.map_or(Action::None, |s| match key.code {
        KeyCode::Tab => {
            s.tab_complete();
            Action::None
        }
        KeyCode::BackTab => {
            s.cycle_completion_back();
            Action::None
        }
        KeyCode::Enter => {
            let was_cycling = !s.completions().is_empty();
            s.accept_completion();
            if was_cycling {
                Action::None
            } else {
                on_confirm
            }
        }
        _ => {
            s.apply_key(key);
            Action::None
        }
    })
}

fn normalize_key(key: KeyEvent) -> KeyEvent {
    match key.code {
        KeyCode::Char(c) if c.is_uppercase() => {
            KeyEvent::new(key.code, key.modifiers | KeyModifiers::SHIFT)
        }
        KeyCode::Char(c)
            if c.is_lowercase() && key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            KeyEvent::new(KeyCode::Char(c.to_ascii_uppercase()), key.modifiers)
        }
        _ => key,
    }
}

fn viewport_start(total: usize, height: usize, scroll: usize) -> usize {
    let skip = scroll.min(total.saturating_sub(height));
    total.saturating_sub(height).saturating_sub(skip)
}

fn push_history(history: &mut VecDeque<u32>, val: u32, max_len: usize) {
    history.push_back(val);
    if history.len() > max_len {
        history.pop_front();
    }
}

fn matches_search(
    entry: &log::Entry,
    re: Option<&regex::Regex>,
    error: bool,
) -> bool {
    if error {
        false
    } else {
        re.is_none_or(|re| re.is_match(entry.message()) || re.is_match(entry.tag()))
    }
}

/// State for the panic backtrace popup, kept as a single unit so a
/// visible popup and its scroll/input state can't exist without a report:
/// the popup is open exactly when `App::backtrace` is `Some` and its
/// `visible` field is `true`.
struct BacktraceState {
    report: backtrace::Report,
    /// Precomputed display lines for `report`, built once per resolve
    /// (see [`ui::backtrace_lines`]) rather than rebuilt on every render.
    lines: Vec<ratatui::text::Line<'static>>,
    visible: bool,
    scroll: usize,
    max_scroll: Cell<usize>,
    elf_input: elf::Selector,
    /// Cached `(content_width, wrapped row count)` for `lines`, since
    /// `lines` is fixed per resolve but the wrapped row count still
    /// depends on the render width. Recomputed only when the width
    /// changes (e.g. the terminal is resized), not on every render.
    wrapped_len_cache: Cell<Option<(u16, usize)>>,
    /// The exact pre-resolve address list `report` was built from. Kept
    /// alongside `report` (rather than reconstructed from its frames on
    /// demand) since that reconstruction is ambiguous: an inlined address
    /// expands to several frames sharing one address, indistinguishable
    /// from a genuine consecutive-duplicate address in the original list.
    addresses: Vec<u64>,
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
    focused_match: Option<usize>,
    pending_backtrace: Option<backtrace::Pending>,
    backtrace: Option<BacktraceState>,
    /// Generation number of the most recently dispatched backtrace resolve
    /// request. See [`Self::next_backtrace_generation`].
    backtrace_generation: u64,
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
            focused_match: None,
            pending_backtrace: None,
            backtrace: None,
            backtrace_generation: 0,
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
            let addresses = backtrace::extract_addresses(entry.message());
            if !addresses.is_empty() && !self.matches_displayed_backtrace(&addresses)
            {
                self.pending_backtrace = Some(backtrace::Pending {
                    header: self.recent_guru_meditation_line(),
                    addresses,
                });
            }
            if self.log_buffer.len() >= self.config.ui.buffer_size {
                let evicted_was_visible = self
                    .log_buffer
                    .front()
                    .is_some_and(|e| self.filter.is_visible(e));
                self.log_buffer.pop_front();
                if evicted_was_visible {
                    self.focused_match =
                        self.focused_match.and_then(|k| k.checked_sub(1));
                }
            }
            let was_visible = self.filter.is_visible(&entry);
            self.log_buffer.push_back(entry);
            if was_visible {
                if let Some(focused) = self.focused_match {
                    let total = self
                        .log_buffer
                        .iter()
                        .filter(|e| self.filter.is_visible(e))
                        .count();
                    self.scroll = total.saturating_sub(focused + 1);
                } else if self.scroll > 0 || !self.filter.search_query().is_empty() {
                    self.scroll = self.scroll.saturating_add(1);
                }
            }
        }
    }

    fn recent_guru_meditation_line(&self) -> Option<String> {
        self.log_buffer
            .iter()
            .rev()
            .take(GURU_MEDITATION_LOOKBACK)
            .find(|e| e.message().contains("Guru Meditation Error"))
            .map(|e| e.message().to_owned())
    }

    /// Returns `true` if `addresses` is the same crash as the currently
    /// displayed backtrace report, so a device re-announcing the same
    /// panic (some ESP-IDF panic handlers print `Backtrace:` more than
    /// once per crash) doesn't force the popup back open after the user
    /// closes it.
    ///
    /// Compares against the exact pre-resolve address list the displayed
    /// report was built from (see [`Self::set_backtrace`]), not a list
    /// reconstructed from its resolved frames: an inlined address expands
    /// to several frames sharing that one address (see
    /// [`backtrace::resolve`]), so collapsing consecutive duplicates back
    /// out is ambiguous whenever the *original* list itself contains a
    /// genuine consecutive repeat (e.g. a corrupted-stack unwind stuck
    /// re-reporting the same return address) — that would be
    /// indistinguishable from inline expansion and could wrongly match a
    /// different, unrelated crash.
    fn matches_displayed_backtrace(&self, addresses: &[u64]) -> bool {
        self.backtrace
            .as_ref()
            .is_some_and(|b| b.addresses == addresses)
    }

    /// Takes the pending backtrace request left by [`Self::push_line`], if
    /// any, so the event loop can dispatch symbol resolution.
    ///
    /// # Returns
    ///
    /// `Some` with the captured addresses and header the first time this is
    /// called after a `Backtrace:` line was seen; `None` otherwise.
    pub(crate) fn take_pending_backtrace(&mut self) -> Option<backtrace::Pending> {
        self.pending_backtrace.take()
    }

    /// Allocates a new backtrace-resolve generation number, superseding
    /// whatever was previously dispatched. Call this once per genuine new
    /// resolve request (a fresh panic, or a manual ELF reload) right
    /// before dispatching it, and carry the returned number alongside the
    /// eventual result so [`Self::apply_backtrace_if_current`] can drop it
    /// if a newer request has since superseded it (e.g. a slow resolve
    /// from an older panic in a crash loop, arriving after a faster
    /// resolve for a subsequent panic already applied).
    ///
    /// # Returns
    ///
    /// The newly allocated generation number.
    pub(crate) fn next_backtrace_generation(&mut self) -> u64 {
        self.backtrace_generation += 1;
        self.backtrace_generation
    }

    /// Applies a resolved backtrace report, unless `generation` has been
    /// superseded by a more recently dispatched resolve request, in which
    /// case the (stale) report is silently dropped.
    ///
    /// # Arguments
    ///
    /// * `generation` - The generation number captured from
    ///   [`Self::next_backtrace_generation`] when this resolve was
    ///   dispatched.
    /// * `addresses` - The exact pre-resolve address list `report` was
    ///   built from.
    /// * `report` - The resolved report to apply if not stale.
    pub(crate) fn apply_backtrace_if_current(
        &mut self,
        generation: u64,
        addresses: Vec<u64>,
        report: backtrace::Report,
    ) {
        if generation == self.backtrace_generation {
            self.set_backtrace(addresses, report);
        }
    }

    /// Stores a decoded backtrace report, (re)computes its display lines,
    /// and opens the backtrace popup.
    ///
    /// The ELF-path input is reseeded from the currently configured
    /// `elf_path` unless the popup was already visible, in which case an
    /// in-progress edit survives a new panic replacing the displayed report.
    ///
    /// # Arguments
    ///
    /// * `addresses` - The exact pre-resolve address list `report` was
    ///   built from, retained for [`Self::matches_displayed_backtrace`] and
    ///   for `load_backtrace_elf`'s reload-against-a-different-ELF flow;
    ///   see [`Self::backtrace_addresses`].
    /// * `report` - The resolved (or best-effort unresolved) backtrace report.
    pub(crate) fn set_backtrace(
        &mut self,
        addresses: Vec<u64>,
        report: backtrace::Report,
    ) {
        let lines = crate::ui::backtrace_lines(&report, &self.config.colors);
        let elf_input = match self.backtrace.take() {
            Some(prev) if prev.visible => prev.elf_input,
            _ => elf::Selector::new(self.elf_path()),
        };
        self.backtrace = Some(BacktraceState {
            report,
            lines,
            visible: true,
            scroll: 0,
            max_scroll: Cell::new(0),
            elf_input,
            wrapped_len_cache: Cell::new(None),
            addresses,
        });
    }

    /// Returns the exact pre-resolve address list the currently displayed
    /// backtrace report was built from, if any.
    ///
    /// # Returns
    ///
    /// `Some` with the addresses, or `None` if no panic has been decoded
    /// this session.
    #[must_use]
    pub(crate) fn backtrace_addresses(&self) -> Option<&[u64]> {
        self.backtrace.as_ref().map(|b| b.addresses.as_slice())
    }

    #[cfg(test)]
    pub(crate) fn set_backtrace_for_test(&mut self, report: backtrace::Report) {
        // Tests seed reports directly rather than through a real resolve,
        // so there's no independently-known pre-resolve address list;
        // reconstructing one from the frames (lossy for a report with a
        // genuine consecutive-duplicate address, same caveat as
        // `matches_displayed_backtrace`'s doc comment) is fine here since
        // none of these tests exercise that ambiguity.
        let mut addresses: Vec<u64> =
            report.frames.iter().map(|f| f.address).collect();
        addresses.dedup();
        self.set_backtrace(addresses, report);
    }

    /// Returns the most recently decoded backtrace report, if any.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the last [`backtrace::Report`], or `None`
    /// if no panic has been decoded this session.
    #[must_use]
    pub(crate) fn backtrace(&self) -> Option<&backtrace::Report> {
        self.backtrace.as_ref().map(|b| &b.report)
    }

    /// Returns the precomputed display lines for the current backtrace
    /// report, if any.
    ///
    /// # Returns
    ///
    /// `Some` with the report's display lines, or `None` if no panic has
    /// been decoded this session.
    #[must_use]
    pub(crate) fn backtrace_lines(&self) -> Option<&[ratatui::text::Line<'static>]> {
        self.backtrace.as_ref().map(|b| b.lines.as_slice())
    }

    /// Returns the number of terminal rows the current backtrace's display
    /// lines occupy once wrapped to `content_width` columns, computing and
    /// caching it on first use for that width and reusing the cached value
    /// on subsequent calls (e.g. every render while the popup stays open at
    /// the same size), rather than re-wrapping on every render.
    ///
    /// # Arguments
    ///
    /// * `content_width` - The render width, in columns, the lines will be
    ///   wrapped to.
    ///
    /// # Returns
    ///
    /// `Some` with the wrapped row count, or `None` if no panic has been
    /// decoded this session.
    #[must_use]
    pub(crate) fn backtrace_wrapped_len(&self, content_width: u16) -> Option<usize> {
        self.backtrace.as_ref().map(|b| {
            if let Some((cached_width, len)) = b.wrapped_len_cache.get() {
                if cached_width == content_width {
                    return len;
                }
            }
            let len = crate::ui::wrapped_row_count(&b.lines, content_width);
            b.wrapped_len_cache.set(Some((content_width, len)));
            len
        })
    }

    /// Returns `true` while the backtrace popup is visible.
    ///
    /// # Returns
    ///
    /// `true` if the backtrace popup is open, `false` otherwise.
    #[must_use]
    pub(crate) fn is_backtrace_open(&self) -> bool {
        self.backtrace.as_ref().is_some_and(|b| b.visible)
    }

    /// Returns the current scroll offset within the backtrace popup.
    ///
    /// # Returns
    ///
    /// Number of lines scrolled down from the top of the report.
    #[must_use]
    pub(crate) fn backtrace_scroll(&self) -> usize {
        self.backtrace.as_ref().map_or(0, |b| b.scroll)
    }

    /// Records the maximum valid scroll offset for the backtrace popup.
    /// Called by the renderer, which knows the viewport height and content
    /// length; uses interior mutability since rendering only borrows `App`
    /// immutably.
    ///
    /// # Arguments
    ///
    /// * `max` - Maximum valid value for `backtrace_scroll`.
    pub(crate) fn set_backtrace_max_scroll(&self, max: usize) {
        if let Some(b) = &self.backtrace {
            b.max_scroll.set(max);
        }
    }

    /// Returns the backtrace popup's ELF-path input, if the popup is open.
    ///
    /// # Returns
    ///
    /// `Some` with a reference to the input, or `None` if the popup is closed.
    #[must_use]
    pub(crate) fn backtrace_elf_input(&self) -> Option<&elf::Selector> {
        self.backtrace.as_ref().map(|b| &b.elf_input)
    }

    /// Returns a mutable reference to the backtrace popup's ELF-path input,
    /// if the popup is open.
    ///
    /// # Returns
    ///
    /// `Some` with a mutable reference to the input, or `None` if the popup
    /// is closed.
    #[cfg(test)]
    pub(crate) fn backtrace_elf_input_mut(&mut self) -> Option<&mut elf::Selector> {
        self.backtrace.as_mut().map(|b| &mut b.elf_input)
    }

    /// Toggles the backtrace popup's visibility; a no-op if no report has
    /// been decoded yet. Reopening reseeds the ELF-path input from the
    /// current `elf_path`, matching [`Self::set_backtrace`]'s rule that the
    /// input is only reset on a closed-to-open transition.
    fn toggle_backtrace_popup(&mut self) {
        // Computed before the mutable borrow below so `elf_path()` (which
        // needs `&self`) doesn't conflict with `state`.
        let elf_path = self.elf_path().map(Path::to_path_buf);
        if let Some(state) = self.backtrace.as_mut() {
            state.visible = if state.visible {
                false
            } else {
                state.elf_input = elf::Selector::new(elf_path.as_deref());
                true
            };
        }
    }

    fn handle_key_backtrace_popup(&mut self, key: KeyEvent) -> Action {
        // For text-input modals, only look up the keymap for non-printable
        // keys (arrows, Esc, modifier combos) so that plain chars still type
        // into the ELF-path input, mirroring `handle_key_elf_selector`. These
        // keymap lookups need `&self` and so are resolved up front, before
        // `state` below takes a mutable borrow of `self.backtrace`.
        let safe = is_modal_safe_key(key);
        let cancel = key.code == KeyCode::Esc || (safe && self.is_cancel_key(key));
        let scroll_up = (safe && self.mapped_to(key, MappableAction::ScrollUp))
            || key.code == KeyCode::Up;
        let scroll_down = (safe && self.mapped_to(key, MappableAction::ScrollDown))
            || key.code == KeyCode::Down;
        let page_up = safe && self.mapped_to(key, MappableAction::PageUp);
        let page_down = safe && self.mapped_to(key, MappableAction::PageDown);

        let Some(state) = self.backtrace.as_mut() else {
            return Action::None;
        };
        if cancel {
            state.visible = false;
            return Action::None;
        }
        // Up/Down navigate the completion dropdown when one is open (matching
        // `handle_key_elf_selector`); otherwise they scroll the frame list.
        if scroll_up {
            if state.elf_input.completions().is_empty() {
                state.scroll = state.scroll.saturating_sub(1);
            } else {
                state.elf_input.move_completion(-1);
            }
            return Action::None;
        }
        if scroll_down {
            if state.elf_input.completions().is_empty() {
                state.scroll =
                    state.scroll.saturating_add(1).min(state.max_scroll.get());
            } else {
                state.elf_input.move_completion(1);
            }
            return Action::None;
        }
        if page_up {
            state.scroll = state.scroll.saturating_sub(10);
            return Action::None;
        }
        if page_down {
            state.scroll =
                state.scroll.saturating_add(10).min(state.max_scroll.get());
            return Action::None;
        }
        handle_elf_input_key(
            Some(&mut state.elf_input),
            key,
            Action::LoadBacktraceElf,
        )
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
        } else if self.is_backtrace_open() {
            self.handle_key_backtrace_popup(key)
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

    fn cancel_filter_popup(&mut self) {
        self.filter.cancel_popup();
        self.focused_match = None;
    }

    fn is_cancel_key(&self, key: KeyEvent) -> bool {
        self.mapped_to(key, MappableAction::QuitPrompt)
            || self.mapped_to(key, MappableAction::Dismiss)
    }

    fn handle_key_quit_confirm(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Char('y') {
            Action::Quit
        } else if key.code == KeyCode::Char('n')
            || key.code == KeyCode::Esc
            || self.is_cancel_key(key)
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
            || self.is_cancel_key(key)
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
            || (safe && self.is_cancel_key(key))
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
        handle_elf_input_key(self.elf_selector.as_mut(), key, Action::ConfirmElfPath)
    }

    fn handle_key_port_selector(&mut self, key: KeyEvent) -> Action {
        let cancel = self.mapped_to(key, MappableAction::ScanPorts)
            || self.is_cancel_key(key)
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
            if self.mapped_to(key, MappableAction::ToggleFilter)
                || key.code == KeyCode::Enter
            {
                self.filter.confirm_popup();
            } else if key.code == KeyCode::Esc {
                if self.filter.search_query().is_empty() {
                    self.cancel_filter_popup();
                } else {
                    self.filter.unfocus_search();
                }
            } else if safe && self.is_cancel_key(key) {
                self.filter.unfocus_search();
            } else if key.code == KeyCode::Up {
                self.filter.unfocus_search();
                self.filter.move_cursor(-1);
            } else if key.code == KeyCode::Down {
                self.filter.unfocus_search();
                self.filter.move_cursor(1);
            } else if self.filter.apply_search_key(key) {
                self.focused_match = None;
            }
        } else if key.code == KeyCode::Enter
            || self.mapped_to(key, MappableAction::ToggleFilter)
        {
            self.filter.confirm_popup();
        } else if key.code == KeyCode::Esc || (safe && self.is_cancel_key(key)) {
            self.cancel_filter_popup();
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
                    self.focused_match = None;
                }
                KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
                    self.filter.toggle_all();
                    self.focused_match = None;
                }
                KeyCode::Backspace => {
                    self.filter.focus_search();
                    if self.filter.apply_search_key(key) {
                        self.focused_match = None;
                    }
                }
                KeyCode::Char(_)
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.filter.focus_search();
                    if self.filter.apply_search_key(key) {
                        self.focused_match = None;
                    }
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

    fn switch_pane(&mut self) {
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
    }

    fn dismiss_action(&mut self) -> Action {
        if self.focused_pane == Pane::Monitor
            && !self.filter.search_query().is_empty()
        {
            self.filter.clear_search();
            self.focused_match = None;
            Action::None
        } else if self.focused_pane == Pane::Monitor && self.scroll > 0 {
            self.scroll = 0;
            Action::None
        } else if self.focused_pane == Pane::Inspector && self.inspector_scroll > 0 {
            self.inspector_scroll = 0;
            Action::None
        } else {
            Action::QuitPrompt
        }
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> Action {
        self.apply_keymap(key)
    }

    fn apply_keymap(&mut self, key: KeyEvent) -> Action {
        let key = normalize_key(key);
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
                self.switch_pane();
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
                    self.filter.open_popup();
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
            Some(MappableAction::QuitPrompt | MappableAction::Dismiss) => {
                self.dismiss_action()
            }
            Some(MappableAction::Flash) => Action::Flash,
            Some(MappableAction::ErasePrompt) => Action::ErasePrompt,
            Some(MappableAction::ResetDevice) => Action::ResetDevice,
            Some(MappableAction::Disconnect) => Action::Disconnect,
            Some(MappableAction::ScanPorts) => Action::ScanPorts,
            Some(MappableAction::SearchNext) => {
                if self.focused_pane == Pane::Monitor {
                    self.search_next();
                }
                Action::None
            }
            Some(MappableAction::SearchPrev) => {
                if self.focused_pane == Pane::Monitor {
                    self.search_prev();
                }
                Action::None
            }
            Some(MappableAction::ToggleBacktrace) => {
                self.toggle_backtrace_popup();
                Action::None
            }
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
        let visible: Vec<&log::Entry> = self
            .log_buffer
            .iter()
            .filter(|e| self.filter.is_visible(e))
            .collect();
        let total = visible.len();
        let start = viewport_start(total, height, self.scroll);
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

    /// Clears the log buffer and resets the scroll offset and search position.
    pub(crate) fn clear_log(&mut self) {
        self.log_buffer.clear();
        self.scroll = 0;
        self.focused_match = None;
    }

    /// Returns the zero-based row index within the current viewport of the
    /// focused search match, if that match is currently visible.
    ///
    /// # Arguments
    ///
    /// * `height` - The number of rows the monitor pane can display.
    ///
    /// # Returns
    ///
    /// `Some(row)` when the focused match is in the visible window,
    /// `None` when no match is focused or the match is scrolled out of view.
    #[must_use]
    pub(crate) fn focused_match_in_window(&self, height: usize) -> Option<usize> {
        let focused = self.focused_match?;
        let total = self
            .log_buffer
            .iter()
            .filter(|e| self.filter.is_visible(e))
            .count();
        let start = viewport_start(total, height, self.scroll);
        (focused >= start && focused < start + height).then(|| focused - start)
    }

    /// Returns `true` if the active regex matches at least one visible entry,
    /// or if no search query is active. Returns `false` only when a valid regex
    /// is present but nothing in the log matches it.
    ///
    /// # Returns
    ///
    /// `false` exclusively when there is a non-empty valid regex with zero
    /// matches in the level+tag-filtered buffer.
    #[must_use]
    pub(crate) fn has_search_matches(&self) -> bool {
        let Some(re) = self.filter.compiled_regex() else {
            return true;
        };
        self.log_buffer
            .iter()
            .filter(|e| self.filter.is_visible(e))
            .any(|e| re.is_match(e.message()) || re.is_match(e.tag()))
    }

    /// Returns the index in the level+tag-filtered entry list of the focused
    /// search match, if any navigation has occurred.
    ///
    /// # Returns
    ///
    /// `Some` with the filtered-list index, or `None` before the first
    /// [`Self::search_next`] or [`Self::search_prev`] call.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn focused_match(&self) -> Option<usize> {
        self.focused_match
    }

    fn search_step(
        &mut self,
        pick: impl Fn(&[usize], Option<usize>) -> Option<usize>,
    ) {
        let re = self.filter.compiled_regex().cloned();
        let error = self.filter.is_regex_error();
        if re.is_none() && !error {
            return;
        }
        let filtered: Vec<&log::Entry> = self
            .log_buffer
            .iter()
            .filter(|e| self.filter.is_visible(e))
            .collect();
        let matches: Vec<usize> = filtered
            .iter()
            .enumerate()
            .filter(|(_, e)| matches_search(e, re.as_ref(), error))
            .map(|(i, _)| i)
            .collect();
        if let Some(idx) = pick(&matches, self.focused_match) {
            self.focused_match = Some(idx);
            self.scroll = filtered.len().saturating_sub(idx + 1);
        }
    }

    fn search_next(&mut self) {
        self.search_step(|matches, cur| {
            let &first = matches.first()?;
            Some(
                cur.and_then(|c| matches.iter().copied().find(|&i| i > c))
                    .unwrap_or(first),
            )
        });
    }

    fn search_prev(&mut self) {
        self.search_step(|matches, cur| {
            let &last = matches.last()?;
            Some(
                cur.and_then(|c| matches.iter().copied().rev().find(|&i| i < c))
                    .unwrap_or(last),
            )
        });
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

    use super::{build_keymap, pick_best_key, GURU_MEDITATION_LOOKBACK};
    use crate::app::{
        format_key_display, Action, App, MappableAction, Pane, DEFAULT_BAUD,
    };
    use crate::config::Config;
    use crate::runner::{
        handle_action, handle_event_message, handle_ports_detected,
    };
    use crate::{backtrace, flash, log};

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
    fn visible_entries_search_does_not_hide_lines() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        app.push_line("E (1) i2c: timeout");
        app.push_line("I (1) wifi: disconnected");
        app.filter_mut().open_popup();
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        assert_eq!(app.visible_entries(10).len(), 3);
    }

    #[test]
    fn visible_entries_search_all_entries_shown_regardless_of_query() {
        let mut app = app();
        app.push_line("I (1) tag: HEAP overflow");
        app.push_line("I (1) tag: stack ok");
        app.filter_mut().open_popup();
        app.filter_mut().push_search_char('h');
        app.filter_mut().push_search_char('e');
        app.filter_mut().push_search_char('a');
        app.filter_mut().push_search_char('p');
        assert_eq!(app.visible_entries(10).len(), 2);
    }

    #[test]
    fn visible_entries_search_by_tag_still_shows_all() {
        let mut app = app();
        app.push_line("I (1) wifi: ok");
        app.push_line("I (1) i2c: ok");
        app.filter_mut().open_popup();
        app.filter_mut().push_search_char('w');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('f');
        app.filter_mut().push_search_char('i');
        assert_eq!(app.visible_entries(10).len(), 2);
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
        app.handle_key(key(KeyCode::Down)); // unfocus search → cursor=1
        app.handle_key(key(KeyCode::Up)); // cursor=0, not focused
        assert!(!app.filter().is_level_hidden(log::Level::Error));
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.filter().is_level_hidden(log::Level::Error));
    }

    #[test]
    fn handle_key_filter_popup_ctrl_a_toggles_all() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Down)); // unfocus search
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
        app.handle_key(key(KeyCode::Down)); // unfocus search → cursor=1
        app.handle_key(key(KeyCode::Up)); // cursor=0, not focused
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
        app.handle_key(key(KeyCode::Down)); // unfocus search
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
    fn handle_action_connect_port_same_port_closes_selector_and_sets_status() {
        let mut app = app_with_port("COM1");
        app.open_port_selector(vec!["COM1".into()]);
        handle_action(&mut app, Action::ConnectPort("COM1".into()), &make_tx());
        assert_eq!(app.port_name(), Some("COM1"));
        assert!(app.port_selector().is_none());
        assert_eq!(app.status_msg(), Some("Connected to COM1."));
    }

    #[test]
    fn handle_action_scan_ports_already_connected_shows_status() {
        let mut app = app_with_port("COM1");
        handle_action(&mut app, Action::ScanPorts, &make_tx());
        assert_eq!(app.port_name(), Some("COM1"));
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

    #[tokio::test]
    async fn handle_action_scan_ports_leaves_app_in_consistent_state() {
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
    fn handle_action_close_elf_selector_does_not_save_draft() {
        let mut app = app();
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector_mut() {
            for ch in "/tmp/app.elf".chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::CloseElfSelector, &make_tx());
        assert_eq!(app.elf_path(), None);
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
    fn handle_event_message_serial_backtrace_without_elf_sets_report_synchronously()
    {
        let mut app = app();
        let tx = make_tx();
        handle_event_message(
            &mut app,
            crate::event::Message::Serial("Backtrace:0x0:0x0".to_owned()),
            DEFAULT_BAUD,
            &tx,
        );
        let report = app.backtrace().unwrap();
        assert!(report.warning.is_some());
        assert_eq!(report.frames.len(), 1);
    }

    #[tokio::test]
    async fn handle_event_message_serial_backtrace_with_elf_resolves_via_channel() {
        let mut app = app();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/panic_test.elf");
        app.set_elf_path(fixture);
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_event_message(
            &mut app,
            crate::event::Message::Serial("Backtrace:0x0:0x0".to_owned()),
            DEFAULT_BAUD,
            &tx,
        );
        let msg = rx.recv().await.unwrap();
        handle_event_message(&mut app, msg, DEFAULT_BAUD, &tx);
        let report = app.backtrace().unwrap();
        assert_eq!(report.frames[0].function.as_deref(), Some("add"));
        assert!(app.is_backtrace_open());
    }

    #[test]
    fn apply_backtrace_if_current_drops_superseded_generation() {
        let mut app = app();
        let stale_gen = app.next_backtrace_generation();
        let current_gen = app.next_backtrace_generation();
        app.apply_backtrace_if_current(
            stale_gen,
            vec![0x1],
            backtrace::Report {
                header: Some("stale".to_owned()),
                frames: Vec::new(),
                warning: None,
            },
        );
        assert!(app.backtrace().is_none());
        app.apply_backtrace_if_current(
            current_gen,
            vec![0x2],
            backtrace::Report {
                header: Some("current".to_owned()),
                frames: Vec::new(),
                warning: None,
            },
        );
        assert_eq!(app.backtrace().unwrap().header.as_deref(), Some("current"));
    }

    #[tokio::test]
    async fn dispatch_pending_backtrace_drops_stale_resolve_applied_out_of_order() {
        // Simulates a crash loop: two panics are detected and dispatched
        // before either resolve completes. Even if the older request's
        // result is applied *after* the newer one's (e.g. a slower
        // resolve for the first panic), the newer report must remain
        // displayed rather than being silently overwritten.
        let mut app = app();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/panic_test.elf");
        app.set_elf_path(fixture);
        let (tx, mut rx) = mpsc::unbounded_channel();

        handle_event_message(
            &mut app,
            crate::event::Message::Serial("Backtrace:0x0:0x0".to_owned()),
            DEFAULT_BAUD,
            &tx,
        );
        let first = rx.recv().await.unwrap();

        handle_event_message(
            &mut app,
            crate::event::Message::Serial("Backtrace:0x400d1fb2:0x0".to_owned()),
            DEFAULT_BAUD,
            &tx,
        );
        let second = rx.recv().await.unwrap();

        // Apply the newer result first, then the stale one arrives late.
        handle_event_message(&mut app, second, DEFAULT_BAUD, &tx);
        let address_after_newer = app.backtrace().unwrap().frames[0].address;
        handle_event_message(&mut app, first, DEFAULT_BAUD, &tx);
        assert_eq!(
            app.backtrace().unwrap().frames[0].address,
            address_after_newer
        );
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
    fn push_line_backtrace_line_populates_pending_request() {
        let mut app = app();
        assert!(app.take_pending_backtrace().is_none());
        app.push_line("Backtrace:0x400d1fb2:0x3ffb2170 0x400d2e9d:0x3ffb2190");
        let pending = app.take_pending_backtrace().unwrap();
        assert_eq!(pending.addresses, vec![0x400d_1fb2, 0x400d_2e9d]);
        assert!(pending.header.is_none());
    }

    #[test]
    fn push_line_backtrace_line_matching_displayed_report_is_ignored() {
        // Regression: some ESP-IDF panic handlers print `Backtrace:` more
        // than once for the same crash. Re-announcing the same addresses
        // must not force the popup back open after the user closes it.
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: None,
            frames: vec![
                backtrace::Frame {
                    address: 0x400d_1fb2,
                    function: None,
                    file: None,
                    line: None,
                },
                backtrace::Frame {
                    address: 0x400d_2e9d,
                    function: None,
                    file: None,
                    line: None,
                },
            ],
            warning: None,
        });
        app.push_line("Backtrace:0x400d1fb2:0x3ffb2170 0x400d2e9d:0x3ffb2190");
        assert!(app.take_pending_backtrace().is_none());
    }

    #[test]
    fn push_line_backtrace_line_matching_displayed_inlined_frames_is_ignored() {
        // Same as above, but the displayed report has multiple frames per
        // address (an inlined chain, see `resolve()`), proving the
        // consecutive-duplicate collapse correctly reconstructs the
        // original per-address list before comparing.
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: None,
            frames: vec![
                backtrace::Frame {
                    address: 0x400d_1fb2,
                    function: Some("inner".to_owned()),
                    file: None,
                    line: None,
                },
                backtrace::Frame {
                    address: 0x400d_1fb2,
                    function: Some("outer".to_owned()),
                    file: None,
                    line: None,
                },
            ],
            warning: None,
        });
        app.push_line("Backtrace:0x400d1fb2:0x3ffb2170");
        assert!(app.take_pending_backtrace().is_none());
    }

    #[test]
    fn push_line_backtrace_line_with_different_addresses_still_pends() {
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: None,
            frames: vec![backtrace::Frame {
                address: 0x400d_1fb2,
                function: None,
                file: None,
                line: None,
            }],
            warning: None,
        });
        app.push_line("Backtrace:0x400d2e9d:0x3ffb2190");
        let pending = app.take_pending_backtrace().unwrap();
        assert_eq!(pending.addresses, vec![0x400d_2e9d]);
    }

    #[test]
    fn push_line_backtrace_line_not_confused_by_displayed_repeated_address() {
        // Regression: comparing against an address list *reconstructed*
        // from the displayed report's frames (collapsing consecutive
        // duplicates) was ambiguous whenever the original pre-resolve
        // list had a genuine consecutive repeat (e.g. a corrupted-stack
        // unwind stuck re-reporting the same return address) -- that
        // collapses to the same shape as a single occurrence of that
        // address, so a later, distinct single-address crash would be
        // wrongly swallowed as "the same" crash. Comparing against the
        // exact stored pre-resolve list (not a reconstruction) fixes it.
        let mut app = app();
        app.set_backtrace(
            vec![0x400d_1fb2, 0x400d_1fb2],
            backtrace::Report {
                header: None,
                frames: vec![
                    backtrace::Frame {
                        address: 0x400d_1fb2,
                        function: None,
                        file: None,
                        line: None,
                    },
                    backtrace::Frame {
                        address: 0x400d_1fb2,
                        function: None,
                        file: None,
                        line: None,
                    },
                ],
                warning: None,
            },
        );
        app.push_line("Backtrace:0x400d1fb2:0x3ffb2170");
        let pending = app.take_pending_backtrace().unwrap();
        assert_eq!(pending.addresses, vec![0x400d_1fb2]);
    }

    #[test]
    fn push_line_non_backtrace_line_leaves_pending_request_empty() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        assert!(app.take_pending_backtrace().is_none());
    }

    #[test]
    fn push_line_backtrace_header_lookback_finds_recent_guru_meditation() {
        let mut app = app();
        app.push_line("Guru Meditation Error: Core 0 panic'ed (LoadProhibited)");
        app.push_line("Register dump:");
        app.push_line("Backtrace:0x400d1fb2:0x3ffb2170");
        let pending = app.take_pending_backtrace().unwrap();
        assert_eq!(
            pending.header.as_deref(),
            Some("Guru Meditation Error: Core 0 panic'ed (LoadProhibited)")
        );
    }

    #[test]
    fn push_line_backtrace_header_lookback_outside_window_is_none() {
        let mut app = app();
        app.push_line("Guru Meditation Error: Core 0 panic'ed (LoadProhibited)");
        for i in 0..GURU_MEDITATION_LOOKBACK {
            app.push_line(&format!("I (1) tag: filler {i}"));
        }
        app.push_line("Backtrace:0x400d1fb2:0x3ffb2170");
        let pending = app.take_pending_backtrace().unwrap();
        assert!(pending.header.is_none());
    }

    #[test]
    fn set_backtrace_precomputes_display_lines() {
        let mut app = app();
        assert!(app.backtrace_lines().is_none());
        app.set_backtrace_for_test(backtrace::Report {
            header: Some("Guru Meditation Error: test".to_owned()),
            frames: vec![backtrace::Frame {
                address: 0x1,
                function: Some("foo".to_owned()),
                file: None,
                line: None,
            }],
            warning: None,
        });
        // Lines exist immediately after set_backtrace, without any render
        // pass having run, confirming they're precomputed once per resolve
        // rather than rebuilt from the report on every render call.
        let lines = app.backtrace_lines().unwrap();
        assert!(!lines.is_empty());
    }

    #[test]
    fn backtrace_wrapped_len_returns_correct_value_for_a_fresh_width() {
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: Some("Guru Meditation Error: test".to_owned()),
            frames: vec![backtrace::Frame {
                address: 0x1,
                function: Some("foo".to_owned()),
                file: None,
                line: None,
            }],
            warning: None,
        });
        let lines = app.backtrace_lines().unwrap().to_vec();
        assert_eq!(
            app.backtrace_wrapped_len(40),
            Some(crate::ui::wrapped_row_count(&lines, 40))
        );
    }

    #[test]
    fn backtrace_wrapped_len_reuses_cached_value_for_the_same_width() {
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: Some("Guru Meditation Error: test".to_owned()),
            frames: vec![backtrace::Frame {
                address: 0x1,
                function: Some("foo".to_owned()),
                file: None,
                line: None,
            }],
            warning: None,
        });
        let real_len = app.backtrace_wrapped_len(40).unwrap();
        // Stuff a deliberately wrong cached value for the same width; if
        // the cache is actually consulted (not recomputed every call),
        // this wrong value comes back unchanged.
        app.backtrace
            .as_ref()
            .unwrap()
            .wrapped_len_cache
            .set(Some((40, real_len + 100)));
        assert_eq!(app.backtrace_wrapped_len(40), Some(real_len + 100));
        // A different width must not reuse that stale entry.
        assert_ne!(app.backtrace_wrapped_len(20), Some(real_len + 100));
    }

    #[test]
    fn set_backtrace_opens_popup_and_resets_scroll() {
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: None,
            frames: Vec::new(),
            warning: None,
        });
        app.set_backtrace_max_scroll(20);
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.backtrace_scroll(), 10);
        // A second panic replaces the report while the popup is already open.
        app.set_backtrace_for_test(backtrace::Report {
            header: None,
            frames: Vec::new(),
            warning: None,
        });
        assert!(app.is_backtrace_open());
        assert_eq!(app.backtrace_scroll(), 0);
        assert!(app.backtrace().is_some());
    }

    #[test]
    fn toggle_backtrace_action_is_noop_without_report() {
        let mut app = app();
        app.apply_keymap(key(KeyCode::Char('b')));
        assert!(!app.is_backtrace_open());
    }

    #[test]
    fn toggle_backtrace_action_toggles_visibility_when_report_present() {
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: None,
            frames: Vec::new(),
            warning: None,
        });
        assert!(app.is_backtrace_open());
        app.apply_keymap(key(KeyCode::Char('b')));
        assert!(!app.is_backtrace_open());
        app.apply_keymap(key(KeyCode::Char('b')));
        assert!(app.is_backtrace_open());
    }

    #[test]
    fn handle_key_backtrace_popup_esc_closes_without_clearing_report() {
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: None,
            frames: Vec::new(),
            warning: None,
        });
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.is_backtrace_open());
        assert!(app.backtrace().is_some());
    }

    fn empty_backtrace_report() -> backtrace::Report {
        backtrace::Report {
            header: None,
            frames: Vec::new(),
            warning: None,
        }
    }

    #[test]
    fn open_backtrace_popup_prefills_elf_input_from_elf_path() {
        let mut app = app();
        app.set_elf_path(std::path::PathBuf::from("/tmp/firmware.elf"));
        app.set_backtrace_for_test(empty_backtrace_report());
        assert_eq!(
            app.backtrace_elf_input().unwrap().value(),
            "/tmp/firmware.elf"
        );
    }

    #[test]
    fn open_backtrace_popup_with_no_elf_path_prefills_empty_input() {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        assert_eq!(app.backtrace_elf_input().unwrap().value(), "");
    }

    #[test]
    fn set_backtrace_does_not_reset_elf_input_when_already_open() {
        let mut app = app();
        app.set_elf_path(std::path::PathBuf::from("/tmp/firmware.elf"));
        app.set_backtrace_for_test(empty_backtrace_report());
        app.backtrace_elf_input_mut()
            .unwrap()
            .apply_key(key(KeyCode::Char('X')));
        // A second panic replaces the report while the popup is already open.
        app.set_backtrace_for_test(empty_backtrace_report());
        assert_eq!(
            app.backtrace_elf_input().unwrap().value(),
            "/tmp/firmware.elfX"
        );
    }

    #[test]
    fn toggle_backtrace_reopen_reinitializes_elf_input_from_current_elf_path() {
        let mut app = app();
        app.set_elf_path(std::path::PathBuf::from("/tmp/old.elf"));
        app.set_backtrace_for_test(empty_backtrace_report());
        app.backtrace_elf_input_mut()
            .unwrap()
            .apply_key(key(KeyCode::Char('X')));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.is_backtrace_open());
        app.set_elf_path(std::path::PathBuf::from("/tmp/new.elf"));
        app.apply_keymap(key(KeyCode::Char('b')));
        assert!(app.is_backtrace_open());
        assert_eq!(app.backtrace_elf_input().unwrap().value(), "/tmp/new.elf");
    }

    #[test]
    fn handle_key_backtrace_popup_char_types_into_elf_input() {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in "/tmp/foo.elf".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(app.backtrace_elf_input().unwrap().value(), "/tmp/foo.elf");
    }

    #[test]
    fn handle_key_backtrace_popup_backspace_and_arrows_edit_elf_input() {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in "/tmp/foo".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Char('X')));
        assert_eq!(app.backtrace_elf_input().unwrap().value(), "/tmp/fXo");
    }

    #[test]
    fn handle_key_backtrace_popup_up_down_pageup_pagedown_scroll_frames_not_box() {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        app.set_backtrace_max_scroll(20);
        for ch in "/tmp/foo.elf".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.backtrace_scroll(), 0);
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.backtrace_scroll(), 2);
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.backtrace_scroll(), 12);
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.backtrace_scroll(), 2);
        assert_eq!(app.backtrace_elf_input().unwrap().value(), "/tmp/foo.elf");
    }

    // Regression: without the `is_modal_safe_key` guard, a bare `q` (bound to
    // `QuitPrompt`, and thus treated as a cancel key) or a preset-remapped
    // scroll letter would fire that action instead of typing into the box.
    #[test]
    fn handle_key_backtrace_popup_letter_bound_to_quit_prompt_still_types() {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in "/tmp/quinn.elf".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        assert!(app.is_backtrace_open());
        assert_eq!(app.backtrace_elf_input().unwrap().value(), "/tmp/quinn.elf");
    }

    fn elf_fixture_dir(name: &str) -> std::path::PathBuf {
        let dir = unique_temp_path(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn handle_key_backtrace_popup_tab_single_match_autocompletes() {
        let dir = elf_fixture_dir("esp-tui-backtrace-popup-single");
        std::fs::write(dir.join("only.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in format!("{}/on", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(
            app.backtrace_elf_input().unwrap().value(),
            dir.join("only.elf").to_str().unwrap()
        );
        assert!(app.backtrace_elf_input().unwrap().completions().is_empty());
    }

    #[test]
    fn handle_key_backtrace_popup_tab_multiple_matches_shows_completions() {
        let dir = elf_fixture_dir("esp-tui-backtrace-popup-multi");
        std::fs::write(dir.join("fw_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        std::fs::write(dir.join("fw_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in format!("{}/fw", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.backtrace_elf_input().unwrap().completions().len(), 2);
    }

    #[test]
    fn handle_key_backtrace_popup_up_down_navigate_completions_when_dropdown_open() {
        let dir = elf_fixture_dir("esp-tui-backtrace-popup-arrow-nav");
        std::fs::write(dir.join("fw_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        std::fs::write(dir.join("fw_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in format!("{}/fw", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        let start = app.backtrace_elf_input().unwrap().completion_cursor();
        app.handle_key(key(KeyCode::Down));
        assert_ne!(
            app.backtrace_elf_input().unwrap().completion_cursor(),
            start
        );
        // Arrow navigation of the dropdown must not also move the frame scroll.
        assert_eq!(app.backtrace_scroll(), 0);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(
            app.backtrace_elf_input().unwrap().completion_cursor(),
            start
        );
    }

    #[test]
    fn handle_key_backtrace_popup_back_tab_cycles_completion_backward() {
        let dir = elf_fixture_dir("esp-tui-backtrace-popup-backtab");
        std::fs::write(dir.join("fw_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        std::fs::write(dir.join("fw_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in format!("{}/fw", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        let forward = app.backtrace_elf_input().unwrap().completion_cursor();
        app.handle_key(key(KeyCode::BackTab));
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(
            app.backtrace_elf_input().unwrap().completion_cursor(),
            forward
        );
    }

    #[test]
    fn handle_key_backtrace_popup_enter_with_completions_accepts_not_loads() {
        let dir = elf_fixture_dir("esp-tui-backtrace-popup-enter-accept");
        std::fs::write(dir.join("fw_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        std::fs::write(dir.join("fw_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in format!("{}/fw", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        assert!(!app.backtrace_elf_input().unwrap().completions().is_empty());
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Action::None);
        assert!(app.backtrace_elf_input().unwrap().completions().is_empty());
        assert!(app.elf_path().is_none());
    }

    #[test]
    fn handle_key_backtrace_popup_enter_without_completions_returns_load_action() {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        for ch in "/tmp/foo.elf".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::LoadBacktraceElf
        );
    }

    #[test]
    fn handle_key_backtrace_popup_esc_discards_draft() {
        let mut app = app();
        app.set_elf_path(std::path::PathBuf::from("/tmp/original.elf"));
        app.set_backtrace_for_test(empty_backtrace_report());
        app.backtrace_elf_input_mut()
            .unwrap()
            .apply_key(key(KeyCode::Char('X')));
        app.handle_key(key(KeyCode::Esc));
        app.apply_keymap(key(KeyCode::Char('b')));
        assert_eq!(
            app.backtrace_elf_input().unwrap().value(),
            "/tmp/original.elf"
        );
    }

    fn seed_backtrace_elf_input(app: &mut App, value: &str) {
        if let Some(s) = app.backtrace_elf_input_mut() {
            for ch in value.chars() {
                s.push_char(ch);
            }
        }
    }

    #[test]
    fn handle_action_load_backtrace_elf_nonexistent_path_sets_status_and_does_not_mutate(
    ) {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        seed_backtrace_elf_input(&mut app, "/nonexistent/path/to.elf");
        handle_action(&mut app, Action::LoadBacktraceElf, &make_tx());
        assert!(app.elf_path().is_none());
        assert!(app.is_backtrace_open());
        assert_eq!(app.status_msg(), Some("Path not found."));
        assert_eq!(app.backtrace().unwrap().frames.len(), 0);
    }

    #[test]
    fn handle_action_load_backtrace_elf_directory_rejected() {
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        seed_backtrace_elf_input(&mut app, std::env::temp_dir().to_str().unwrap());
        handle_action(&mut app, Action::LoadBacktraceElf, &make_tx());
        assert!(app.elf_path().is_none());
        assert!(app.is_backtrace_open());
        assert_eq!(app.status_msg(), Some("Path is a directory."));
    }

    #[test]
    fn handle_action_load_backtrace_elf_non_elf_rejected() {
        let path = unique_temp_path("esp-tui-backtrace-popup-not-elf");
        std::fs::write(&path, b"not an elf file").unwrap();
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        seed_backtrace_elf_input(&mut app, path.to_str().unwrap());
        handle_action(&mut app, Action::LoadBacktraceElf, &make_tx());
        assert!(app.elf_path().is_none());
        assert!(app.is_backtrace_open());
        assert_eq!(app.status_msg(), Some("Not a valid ELF file."));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn handle_action_load_backtrace_elf_valid_updates_elf_path_and_reresolves()
    {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/panic_test.elf");
        let mut app = app();
        app.set_backtrace_for_test(backtrace::Report {
            header: Some("Guru Meditation Error: Core 0 panic'ed".to_owned()),
            frames: vec![backtrace::Frame {
                address: 0x0,
                function: None,
                file: None,
                line: None,
            }],
            warning: Some("No ELF file loaded; showing raw addresses.".to_owned()),
        });
        seed_backtrace_elf_input(&mut app, fixture.to_str().unwrap());
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_action(&mut app, Action::LoadBacktraceElf, &tx);
        assert_eq!(app.elf_path(), Some(fixture.as_path()));
        let msg = rx.recv().await.unwrap();
        handle_event_message(&mut app, msg, DEFAULT_BAUD, &tx);
        let report = app.backtrace().unwrap();
        assert_eq!(report.frames[0].function.as_deref(), Some("add"));
        assert!(report.warning.is_none());
        assert!(app.is_backtrace_open());
    }

    #[tokio::test]
    async fn handle_action_load_backtrace_elf_valid_updates_prefill_seen_by_flash_selector(
    ) {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/panic_test.elf");
        let mut app = app();
        app.set_backtrace_for_test(empty_backtrace_report());
        seed_backtrace_elf_input(&mut app, fixture.to_str().unwrap());
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_action(&mut app, Action::LoadBacktraceElf, &tx);
        let msg = rx.recv().await.unwrap();
        handle_event_message(&mut app, msg, DEFAULT_BAUD, &tx);
        let prefill = app.elf_path().map(std::path::Path::to_path_buf);
        app.open_elf_selector(prefill.as_deref());
        assert_eq!(
            app.elf_selector().unwrap().value(),
            fixture.to_str().unwrap()
        );
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
            "j"
        );
        assert_eq!(
            format_key_display(KeyCode::Char('N'), KeyModifiers::SHIFT),
            "N"
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
            "old q → QuitPrompt binding should be removed when QuitPrompt is remapped"
        );
        assert!(
            map.contains_key(&(KeyCode::Esc, KeyModifiers::empty())),
            "Esc maps to Dismiss — should not be removed"
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

    #[test]
    fn ctrl_f_opens_filter_popup_in_default_keymap() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('f')));
        assert!(app.filter().is_popup_open());
    }

    #[test]
    fn search_next_no_op_when_query_empty() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), None);
    }

    #[test]
    fn search_next_no_op_on_invalid_regex() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        app.filter_mut().push_search_char('[');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), None);
    }

    #[test]
    fn search_next_finds_first_match() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        app.push_line("E (1) i2c: timeout");
        app.push_line("I (1) wifi: reconnected");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(1));
    }

    #[test]
    fn search_next_advances_to_next_match() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("E (1) i2c: ok");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(2));
    }

    #[test]
    fn search_next_wraps_to_first_after_last() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("E (1) i2c: ok");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(2));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
    }

    #[test]
    fn search_prev_starts_at_last_when_no_focused_match() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("E (1) i2c: ok");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
        assert_eq!(app.focused_match(), Some(2));
    }

    #[test]
    fn search_prev_goes_to_previous_match() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("E (1) i2c: ok");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(2));
        app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
        assert_eq!(app.focused_match(), Some(0));
    }

    #[test]
    fn search_prev_wraps_to_last_from_first() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("E (1) i2c: ok");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
        assert_eq!(app.focused_match(), Some(2));
    }

    #[test]
    fn search_nav_skips_level_filtered_entries() {
        let mut app = app();
        app.push_line("E (1) tag: timeout error");
        app.push_line("I (1) tag: timeout info");
        app.filter_mut().toggle_at_cursor();
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
        let entries: Vec<&log::Entry> = app
            .log_buffer
            .iter()
            .filter(|e| app.filter().is_visible(e))
            .collect();
        assert_eq!(entries[0].message(), "timeout info");
    }

    #[test]
    fn search_next_regex_alternation() {
        let mut app = app();
        app.push_line("I (1) wifi: connected");
        app.push_line("E (1) i2c: timeout");
        app.push_line("I (1) uart: ok");
        for c in "wifi|i2c".chars() {
            app.filter_mut().push_search_char(c);
        }
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(1));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
    }

    #[test]
    fn focused_match_resets_on_query_change() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.focused_match().is_some());
        app.filter_mut().open_popup();
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.focused_match(), None);
    }

    #[test]
    fn focused_match_does_not_reset_on_cursor_move_in_popup() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.focused_match().is_some());
        app.filter_mut().open_popup();
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Up));
        assert!(app.focused_match().is_some());
    }

    #[test]
    fn focused_match_resets_on_clear_log() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.focused_match().is_some());
        app.handle_key(ctrl(KeyCode::Char('l')));
        assert_eq!(app.focused_match(), None);
    }

    #[test]
    fn focused_match_resets_on_level_toggle_in_popup() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout");
        app.push_line("E (1) i2c: timeout");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.focused_match().is_some());
        app.filter_mut().open_popup();
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.focused_match(), None);
    }

    #[test]
    fn focused_match_in_window_returns_none_when_no_focused_match() {
        let app = app();
        assert_eq!(app.focused_match_in_window(10), None);
    }

    #[test]
    fn focused_match_in_window_returns_row_after_search_next() {
        let mut app = app();
        for i in 0..5 {
            app.push_line(&format!("I (1) wifi: line {i}"));
        }
        app.push_line("E (1) i2c: timeout target");
        for i in 0..4 {
            app.push_line(&format!("I (1) wifi: line {i}"));
        }
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('a');
        app.filter_mut().push_search_char('r');
        app.filter_mut().push_search_char('g');
        app.filter_mut().push_search_char('e');
        app.filter_mut().push_search_char('t');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(5));
        let row = app.focused_match_in_window(10);
        assert!(row.is_some());
    }

    #[test]
    fn focused_match_in_window_returns_none_when_scrolled_out() {
        let mut app = app();
        for i in 0..20 {
            app.push_line(&format!("I (1) wifi: line {i}"));
        }
        app.push_line("E (1) i2c: target");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('a');
        app.filter_mut().push_search_char('r');
        app.filter_mut().push_search_char('g');
        app.filter_mut().push_search_char('e');
        app.filter_mut().push_search_char('t');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(20));
        for _ in 0..15 {
            app.handle_key(key(KeyCode::Up));
        }
        assert_eq!(app.focused_match_in_window(5), None);
    }

    #[test]
    fn push_line_scroll_drifts_when_search_active_and_scrolled() {
        let mut app = app();
        for i in 0..10 {
            app.push_line(&format!("I (1) wifi: line {i}"));
        }
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.push_line("I (1) wifi: unrelated");
        assert_eq!(app.scroll(), 2, "scroll drifts for all visible lines");
    }

    #[test]
    fn push_line_scroll_freezes_at_bottom_when_search_active() {
        let mut app = app();
        for i in 0..5 {
            app.push_line(&format!("I (1) wifi: line {i}"));
        }
        assert_eq!(app.scroll(), 0, "starts at bottom");
        app.filter_mut().push_search_char('l');
        app.push_line("I (1) wifi: line 5");
        assert_eq!(
            app.scroll(),
            1,
            "search freezes viewport; new entry doesn't pull view down"
        );
    }

    #[test]
    fn push_line_scroll_tracks_focused_match() {
        let mut app = app();
        app.push_line("I (1) tag: match me");
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
        assert_eq!(
            app.scroll(),
            0,
            "single entry: scroll is 0 after search_next"
        );
        app.push_line("I (1) tag: new entry");
        assert_eq!(
            app.scroll(),
            1,
            "scroll must increase to keep focused match in view as new lines arrive"
        );
    }

    #[test]
    fn focused_match_decrements_on_visible_eviction() {
        let mut cfg = Config::default();
        cfg.ui.buffer_size = 3;
        let mut app = App::new(None, cfg);
        app.push_line("I (1) tag: match one");
        app.push_line("I (1) tag: match two");
        app.push_line("I (1) tag: match three");
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(1));
        app.push_line("I (1) tag: match four");
        assert_eq!(
            app.focused_match(),
            Some(0),
            "evicting the first visible entry decrements focused_match"
        );
    }

    #[test]
    fn focused_match_clears_when_eviction_removes_match_at_index_zero() {
        let mut cfg = Config::default();
        cfg.ui.buffer_size = 3;
        let mut app = App::new(None, cfg);
        app.push_line("I (1) tag: match one");
        app.push_line("I (1) tag: match two");
        app.push_line("I (1) tag: match three");
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focused_match(), Some(0));
        app.push_line("I (1) tag: match four");
        assert_eq!(
            app.focused_match(),
            None,
            "evicting the focused entry clears focused_match"
        );
    }

    #[test]
    fn dismiss_clears_search_before_scrolling() {
        let mut app = app();
        for i in 0..5 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        app.handle_key(key(KeyCode::Up));
        app.filter_mut().push_search_char('l');
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.filter().search_query(), "");
        assert!(app.scroll() > 0, "scroll unchanged; search cleared first");
    }

    #[test]
    fn dismiss_exits_scroll_when_no_search() {
        let mut app = app();
        for i in 0..5 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        app.handle_key(key(KeyCode::Up));
        assert!(app.filter().search_query().is_empty());
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn dismiss_opens_quit_prompt_when_idle() {
        let mut app = app();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::QuitPrompt);
    }

    #[test]
    fn q_clears_search_in_default_keymap() {
        let mut app = app();
        app.filter_mut().push_search_char('l');
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::None);
        assert_eq!(app.filter().search_query(), "");
    }

    #[test]
    fn has_search_matches_returns_true_when_no_query() {
        let mut app = app();
        app.push_line("I (1) tag: hello");
        assert!(app.has_search_matches());
    }

    #[test]
    fn has_search_matches_returns_true_when_match_exists() {
        let mut app = app();
        app.push_line("I (1) tag: hello");
        app.filter_mut().push_search_char('h');
        assert!(app.has_search_matches());
    }

    #[test]
    fn has_search_matches_returns_false_when_no_match() {
        let mut app = app();
        app.push_line("I (1) tag: hello");
        app.filter_mut().push_search_char('z');
        assert!(!app.has_search_matches());
    }

    #[test]
    fn n_key_triggers_search_next() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.focused_match().is_some());
    }

    #[test]
    fn shift_n_key_triggers_search_prev() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
        assert_eq!(app.focused_match(), Some(1));
    }

    #[test]
    fn uppercase_n_without_shift_modifier_triggers_search_prev() {
        let mut app = app();
        app.push_line("I (1) wifi: timeout one");
        app.push_line("I (1) wifi: timeout two");
        app.filter_mut().push_search_char('t');
        app.filter_mut().push_search_char('i');
        app.filter_mut().push_search_char('m');
        app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::empty()));
        assert_eq!(app.focused_match(), Some(1));
    }
}

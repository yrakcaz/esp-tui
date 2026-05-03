use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, Duration};

use crate::{demo, elf, event, filter, flash, log, port, serial, ui};

const BUFFER_SIZE: usize = 10_000;
const STATUS_TTL_SECS: u64 = 3;
const DEFAULT_BAUD: u32 = 115_200;

#[derive(Parser)]
#[command(name = "esp-tui", about = "ESP32 developer TUI")]
struct Args {
    /// Serial port to connect to.
    #[arg(long, short)]
    port: Option<String>,

    /// Run in demo mode with synthetic log output (no hardware required).
    #[arg(long)]
    demo: bool,

    /// Path to the ELF firmware file to flash.
    #[arg(long)]
    elf: Option<PathBuf>,

    /// Serial baud rate.
    #[arg(long, short = 'b')]
    baud: Option<u32>,
}

/// Outcome of a keypress that requires I/O, returned to the event loop to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// Central application state.
pub(crate) struct App {
    log_buffer: VecDeque<log::Entry>,
    scroll: usize,
    filter: filter::State,
    port_name: Option<String>,
    port_cmd_tx: Option<std::sync::mpsc::Sender<serial::PortCommand>>,
    source_shutdown_tx: Option<watch::Sender<bool>>,
    status_msg: Option<(String, Instant)>,
    running: bool,
    port_selector: Option<port::Selector>,
    demo: bool,
    flash_state: flash::State,
    device_info: Option<flash::DeviceInfo>,
    erase_confirm: bool,
    elf_path: Option<PathBuf>,
    elf_selector: Option<elf::Selector>,
    baud: u32,
}

impl App {
    /// Creates a new application state.
    ///
    /// # Arguments
    ///
    /// * `port_name` - The connected serial port name, if already known.
    ///
    /// # Returns
    ///
    /// An [`App`] with an empty log buffer, all filters visible, and the event
    /// loop running.
    #[must_use]
    pub(crate) fn new(port_name: Option<String>) -> Self {
        Self {
            log_buffer: VecDeque::new(),
            scroll: 0,
            filter: filter::State::new(),
            port_name,
            port_cmd_tx: None,
            source_shutdown_tx: None,
            status_msg: None,
            running: true,
            port_selector: None,
            demo: false,
            flash_state: flash::State::Idle,
            device_info: None,
            erase_confirm: false,
            elf_path: None,
            elf_selector: None,
            baud: DEFAULT_BAUD,
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
            if self.log_buffer.len() >= BUFFER_SIZE {
                self.log_buffer.pop_front();
            }
            if self.scroll > 0 && self.filter.is_visible(&entry) {
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
    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }

        if self.erase_confirm {
            return match key.code {
                KeyCode::Char('y') => Action::ConfirmErase,
                KeyCode::Char('n' | 'q') | KeyCode::Esc => {
                    self.erase_confirm = false;
                    Action::None
                }
                _ => Action::None,
            };
        }

        if self.elf_selector.is_some() {
            return match key.code {
                KeyCode::Esc => Action::CloseElfSelector,
                KeyCode::Enter => {
                    if self
                        .elf_selector
                        .as_ref()
                        .is_some_and(|s| !s.completions().is_empty())
                    {
                        if let Some(s) = self.elf_selector.as_mut() {
                            s.accept_completion();
                        }
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
                KeyCode::Up => {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.move_completion(-1);
                    }
                    Action::None
                }
                KeyCode::Down => {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.move_completion(1);
                    }
                    Action::None
                }
                KeyCode::Left => {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.move_cursor(-1);
                    }
                    Action::None
                }
                KeyCode::Right => {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.move_cursor(1);
                    }
                    Action::None
                }
                KeyCode::Backspace => {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.backspace();
                    }
                    Action::None
                }
                KeyCode::Char('a')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.move_cursor_to_start();
                    }
                    Action::None
                }
                KeyCode::Char('e')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.move_cursor_to_end();
                    }
                    Action::None
                }
                KeyCode::Char('l')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.clear_input();
                    }
                    Action::None
                }
                KeyCode::Char('d')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.delete_forward();
                    }
                    Action::None
                }
                KeyCode::Char('k')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.kill_to_end();
                    }
                    Action::None
                }
                KeyCode::Char('u')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.kill_to_start();
                    }
                    Action::None
                }
                KeyCode::Char('w')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.kill_word_back();
                    }
                    Action::None
                }
                KeyCode::Char(ch)
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(s) = self.elf_selector.as_mut() {
                        s.push_char(ch);
                    }
                    Action::None
                }
                _ => Action::None,
            };
        }

        if self.port_selector.is_some() {
            return match key.code {
                KeyCode::Up => {
                    if let Some(s) = self.port_selector.as_mut() {
                        s.move_cursor(-1);
                    }
                    Action::None
                }
                KeyCode::Down => {
                    if let Some(s) = self.port_selector.as_mut() {
                        s.move_cursor(1);
                    }
                    Action::None
                }
                KeyCode::Enter => {
                    self.port_selector.take().map_or(Action::None, |s| {
                        Action::ConnectPort(s.selected().to_owned())
                    })
                }
                KeyCode::Char('q' | 'c') | KeyCode::Esc => {
                    self.port_selector = None;
                    Action::None
                }
                _ => Action::None,
            };
        }

        if self.filter.is_popup_open() {
            match key.code {
                KeyCode::Up => self.filter.move_cursor(-1),
                KeyCode::Down => self.filter.move_cursor(1),
                KeyCode::Char(' ') => self.filter.toggle_at_cursor(),
                KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
                    self.filter.toggle_all();
                }
                KeyCode::Tab | KeyCode::Esc | KeyCode::Char('q') => {
                    self.filter.toggle_popup();
                }
                _ => {}
            }
            return Action::None;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc if self.scroll > 0 => {
                self.scroll = 0;
                Action::None
            }
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Char('d') => Action::Disconnect,
            KeyCode::Char('r') => Action::ResetDevice,
            KeyCode::Char('f') => Action::Flash,
            KeyCode::Char('e') => Action::ErasePrompt,
            KeyCode::Char('c') => Action::ScanPorts,
            KeyCode::Tab => {
                self.filter.toggle_popup();
                Action::None
            }
            KeyCode::Char('l') if key.modifiers == KeyModifiers::CONTROL => {
                self.clear_log();
                Action::None
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_add(1);
                Action::None
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_sub(1);
                Action::None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(10);
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Sets an ephemeral status message that expires after a few seconds.
    ///
    /// # Arguments
    ///
    /// * `msg` - The message to display in the status bar.
    pub(crate) fn set_status(&mut self, msg: String) {
        self.status_msg = Some((msg, Instant::now()));
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
        let skip = self.scroll.min(total.saturating_sub(height));
        let start = total.saturating_sub(height).saturating_sub(skip);
        visible[start..total.min(start + height)].to_vec()
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

    /// Sets the connected port name and clears the port selector.
    ///
    /// # Arguments
    ///
    /// * `port` - The port name to use going forward.
    pub(crate) fn set_port(&mut self, port: String) {
        self.port_name = Some(port);
        self.port_selector = None;
        self.port_cmd_tx = None;
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
    }

    /// Returns `true` when the application is running in demo mode.
    ///
    /// # Returns
    ///
    /// `true` if demo mode is active, `false` otherwise.
    #[must_use]
    pub(crate) fn is_demo(&self) -> bool {
        self.demo
    }

    /// Puts the application into demo mode, sealing it from real hardware.
    pub(crate) fn set_demo(&mut self) {
        self.demo = true;
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

    /// Returns `true` while a flash or erase operation is in progress.
    ///
    /// # Returns
    ///
    /// `true` if state is `Flashing` or `Erasing`, `false` otherwise.
    #[must_use]
    pub(crate) fn is_flashing(&self) -> bool {
        matches!(
            self.flash_state,
            flash::State::Flashing { .. } | flash::State::Erasing
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
        self.erase_confirm
    }

    /// Opens the erase confirmation prompt.
    pub(crate) fn open_erase_confirm(&mut self) {
        self.erase_confirm = true;
    }

    /// Closes the erase confirmation prompt.
    pub(crate) fn close_erase_confirm(&mut self) {
        self.erase_confirm = false;
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
}

fn begin_connect(port: &str, baud: u32, tx: &mpsc::UnboundedSender<event::Message>) {
    let (src_tx, src_rx) = watch::channel(false);
    let port_name = port.to_owned();
    let tx_task = tx.clone();
    drop(tokio::task::spawn_blocking(move || {
        let probe = flash::probe_device_info(&port_name, baud);
        let _ = tx_task.send(event::Message::DeviceInfo(probe));
        // Let the OS release the file descriptor before opening for serial reads.
        std::thread::sleep(std::time::Duration::from_millis(50));
        serial::Port::new(&port_name, baud)
            .connect_and_read(&tx_task, &src_rx, src_tx);
    }));
}

fn resolve_ports(port_arg: Option<String>) -> anyhow::Result<Vec<String>> {
    port_arg.map_or_else(serial::detect_esp_ports, |p| Ok(vec![p]))
}

fn apply_scan(app: &mut App, tx: &mpsc::UnboundedSender<event::Message>) {
    match serial::detect_esp_ports() {
        Err(e) => app.set_status(format!("Port scan failed: {e}")),
        Ok(ports) if ports.is_empty() => {
            app.set_status("No devices detected.".into());
        }
        Ok(mut ports) if ports.len() == 1 => {
            let port = ports.remove(0);
            app.set_status(format!("Connecting to {port}..."));
            begin_connect(&port, app.baud(), tx);
        }
        Ok(ports) => app.open_port_selector(ports),
    }
}

fn handle_ports_detected(
    app: &mut App,
    mut current: Vec<String>,
    previous: &[String],
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    if !app.is_demo() && !app.is_flashing() {
        if app.port_name().is_none() {
            if app.port_selector().is_some() {
                match current.len() {
                    0 => {
                        app.close_port_selector();
                        app.set_status("No devices detected.".into());
                    }
                    1 => {
                        app.close_port_selector();
                        app.set_status(format!("Connecting to {}...", current[0]));
                        begin_connect(&current[0], app.baud(), tx);
                    }
                    _ => app.refresh_port_selector(current),
                }
            } else {
                match current.len() {
                    0 => {}
                    1 => {
                        let port = current.remove(0);
                        app.set_status(format!("Connecting to {port}..."));
                        begin_connect(&port, app.baud(), tx);
                    }
                    _ => app.open_port_selector(current),
                }
            }
        } else {
            let connected_present = app
                .port_name()
                .is_some_and(|n| current.iter().any(|p| p.as_str() == n));
            let has_new_port = current.iter().any(|p| !previous.contains(p));
            if has_new_port && connected_present {
                app.set_status("New device detected. Press [c] to connect.".into());
            }
        }
    }
}

fn spawn_port_poller(
    tx: mpsc::UnboundedSender<event::Message>,
    mut shutdown: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut last_ports: Vec<String> = Vec::new();
        let mut poll = interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    if let Ok(Ok(ports)) =
                        tokio::task::spawn_blocking(serial::detect_esp_ports).await
                    {
                        if ports != last_ports {
                            let previous = std::mem::replace(&mut last_ports, ports.clone());
                            if tx
                                .send(event::Message::PortsDetected {
                                    current: ports,
                                    previous,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    });
}

fn confirm_elf_path(app: &mut App, tx: &mpsc::UnboundedSender<event::Message>) {
    let value = app
        .elf_selector()
        .map(|s| s.value().to_owned())
        .unwrap_or_default();
    let path = PathBuf::from(&value);
    if path.is_dir() {
        app.set_status("Path is a directory.".into());
    } else if !path.is_file() {
        app.set_status("Path not found.".into());
    } else if !elf::is_elf_file(&path) {
        app.set_status("Not a valid ELF file.".into());
    } else {
        app.set_elf_path(path);
        app.close_elf_selector();
        do_flash(app, tx);
    }
}

fn start_flash(app: &mut App, _tx: &mpsc::UnboundedSender<event::Message>) {
    let prefill = app.elf_path().map(Path::to_path_buf);
    app.open_elf_selector(prefill.as_deref());
}

fn do_flash(app: &mut App, tx: &mpsc::UnboundedSender<event::Message>) {
    if app.port_name().is_none() {
        app.set_status("No port connected.".into());
    } else if app.is_flashing() {
        app.set_status("Flash already in progress.".into());
    } else {
        let port = app.port_name().unwrap().to_owned();
        let baud = app.baud();
        let elf_path = app.elf_path().unwrap().to_owned();
        app.shutdown_source();
        app.set_flash_state(flash::State::Flashing {
            current: 0,
            total: 0,
        });
        let tx_task = tx.clone();
        drop(tokio::task::spawn_blocking(move || {
            let result = flash::flash_elf(&port, baud, &elf_path, tx_task.clone());
            let _ = tx_task.send(event::Message::FlashDone(result));
        }));
    }
}

fn handle_action(
    app: &mut App,
    action: Action,
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    match action {
        Action::None => {}
        Action::Quit => app.quit(),
        Action::Disconnect => {
            if app.port_name().is_some() {
                app.disconnect();
                app.set_status("Disconnected.".into());
            } else {
                app.set_status("Not connected.".into());
            }
        }
        Action::CloseElfSelector => app.close_elf_selector(),
        Action::ConfirmElfPath => confirm_elf_path(app, tx),
        // All actions below touch real hardware. New hardware actions must go
        // after this arm so demo mode blocks them automatically.
        _ if app.is_demo() => {
            app.set_status("Not available in demo mode.".into());
        }
        Action::ResetDevice => match app.port_cmd_tx() {
            Some(cmd_tx) => {
                if cmd_tx.send(serial::PortCommand::Reset).is_err() {
                    app.set_status("Reset failed: port disconnected.".into());
                } else {
                    app.set_status("Reset sent.".into());
                }
            }
            None if app.port_name().is_some() => {
                app.set_status("Reset not supported.".into());
            }
            None => app.set_status("No port connected.".into()),
        },
        Action::ScanPorts => apply_scan(app, tx),
        Action::ConnectPort(port) => {
            app.set_status(format!("Connecting to {port}..."));
            begin_connect(&port, app.baud(), tx);
        }
        Action::ErasePrompt => {
            if app.port_name().is_none() {
                app.set_status("No port connected.".into());
            } else if app.is_flashing() {
                app.set_status("Operation already in progress.".into());
            } else {
                app.open_erase_confirm();
            }
        }
        Action::ConfirmErase => {
            app.close_erase_confirm();
            if let Some(port) = app.port_name().map(str::to_owned) {
                let baud = app.baud();
                app.shutdown_source();
                app.set_flash_state(flash::State::Erasing);
                let tx_task = tx.clone();
                drop(tokio::task::spawn_blocking(move || {
                    let result = flash::erase_flash(&port, baud);
                    let _ = tx_task.send(event::Message::EraseDone(result));
                }));
            }
        }
        Action::Flash => start_flash(app, tx),
    }
}

#[allow(clippy::too_many_lines)]
async fn run_inner(args: Args) -> anyhow::Result<()> {
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal =
        Terminal::new(backend).context("failed to create terminal")?;

    let (tx, mut rx) = mpsc::unbounded_channel::<event::Message>();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let baud = args.baud.unwrap_or(DEFAULT_BAUD);
    let mut app = App::new(None);
    app.set_baud(baud);
    if let Some(path) = args.elf {
        app.set_elf_path(path);
    }

    if args.demo {
        app.set_demo();
        let (src_tx, src_rx) = watch::channel(false);
        demo::spawn(tx.clone(), src_rx);
        demo::spawn_device_info(tx.clone());
        app.set_port("demo".into());
        app.set_source_shutdown(src_tx);
        app.set_status("Connected to demo.".into());
    } else {
        let mut ports = resolve_ports(args.port)?;
        match ports.len() {
            0 => {}
            1 => {
                let port = ports.remove(0);
                app.set_status(format!("Connecting to {port}..."));
                begin_connect(&port, baud, &tx);
            }
            _ => app.open_port_selector(ports),
        }
        spawn_port_poller(tx.clone(), shutdown_rx.clone());
    }

    let mut tick = interval(Duration::from_millis(250));

    let key_tx = tx.clone();
    let key_shutdown = shutdown_rx.clone();
    tokio::task::spawn_blocking(move || loop {
        if *key_shutdown.borrow() {
            break;
        }
        match crossterm::event::poll(std::time::Duration::from_millis(50)) {
            Ok(true) => match crossterm::event::read() {
                Ok(Event::Key(key)) => {
                    if key_tx.send(event::Message::Key(key)).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    });

    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        let msg = tokio::select! {
            Some(m) = rx.recv() => m,
            _ = tick.tick() => event::Message::Tick,
        };

        match msg {
            event::Message::Key(key) => {
                let action = app.handle_key(key);
                handle_action(&mut app, action, &tx);
            }
            event::Message::Serial(line) => app.push_line(&line),
            event::Message::Disconnected => {
                app.disconnect();
                app.set_status("Disconnected.".into());
            }
            event::Message::ConnectSuccess {
                port,
                cmd_tx,
                src_tx,
            } => {
                let status = format!("Connected to {port}.");
                app.set_port(port);
                app.set_port_cmd(cmd_tx);
                app.set_source_shutdown(src_tx);
                app.set_status(status);
            }
            event::Message::ConnectError(msg) => {
                app.set_status(msg);
            }
            event::Message::Tick => app.tick(),
            event::Message::PortsDetected { current, previous } => {
                handle_ports_detected(&mut app, current, &previous, &tx);
            }
            event::Message::FlashProgress { current, total } => {
                app.set_flash_state(flash::State::Flashing { current, total });
            }
            event::Message::FlashDone(result) => {
                app.set_flash_state(flash::State::Idle);
                match result {
                    Ok(()) => {
                        app.set_status("Flash complete. Reconnecting...".into());
                        if let Some(port) = app.port_name().map(str::to_owned) {
                            begin_connect(&port, baud, &tx);
                        }
                    }
                    Err(e) => {
                        app.set_status(format!("Flash failed: {e}"));
                        if let Some(port) = app.port_name().map(str::to_owned) {
                            begin_connect(&port, baud, &tx);
                        }
                    }
                }
            }
            event::Message::DeviceInfo(result) => {
                if let Ok(info) = result {
                    app.set_device_info(info);
                }
            }
            event::Message::EraseDone(result) => {
                app.set_flash_state(flash::State::Idle);
                match result {
                    Ok(()) => {
                        app.set_status("Erase complete.".into());
                    }
                    Err(e) => {
                        app.set_status(format!("Erase failed: {e}"));
                    }
                }
                if let Some(port) = app.port_name().map(str::to_owned) {
                    begin_connect(&port, baud, &tx);
                }
            }
        }

        if !app.is_running() {
            break;
        }
    }

    app.shutdown_source();
    let _ = shutdown_tx.send(true);
    Ok(())
}

/// Runs the application: parses CLI arguments, initialises the terminal, and
/// drives the event loop until the user quits.
///
/// # Errors
///
/// Returns an error if terminal initialisation or any I/O operation fails.
pub(crate) async fn run() -> anyhow::Result<()> {
    let args = Args::parse();

    enable_raw_mode().context("failed to enable raw mode")?;
    std::io::stdout()
        .execute(EnterAlternateScreen)
        .context("failed to enter alternate screen")?;

    let result = run_inner(args).await;

    let _ = disable_raw_mode();
    let _ = std::io::stdout().execute(LeaveAlternateScreen);

    result
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc;

    use super::{handle_action, handle_ports_detected, Action, App, BUFFER_SIZE};
    use crate::log;

    fn make_tx() -> mpsc::UnboundedSender<crate::event::Message> {
        let (tx, _) = mpsc::unbounded_channel();
        tx
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn app_initial_state() {
        let app = App::new(Some("COM1".into()));
        assert!(app.is_running());
        assert_eq!(app.port_name(), Some("COM1"));
        assert_eq!(app.scroll(), 0);
        assert!(app.status_msg().is_none());
        assert!(app.port_selector().is_none());
        assert!(!app.is_demo(), "new App must not start in demo mode");
        assert!(!app.is_flashing());
        assert!(app.device_info().is_none());
        assert!(!app.is_erase_confirm_open());
        assert!(app.elf_path().is_none());
    }

    #[test]
    fn app_new_no_port() {
        let app = App::new(None);
        assert!(app.port_name().is_none());
    }

    #[test]
    fn app_quit_stops_running() {
        let mut app = App::new(None);
        app.quit();
        assert!(!app.is_running());
    }

    #[test]
    fn app_set_status_and_read() {
        let mut app = App::new(None);
        app.set_status("hello".into());
        assert_eq!(app.status_msg(), Some("hello"));
    }

    #[test]
    fn tick_no_status_is_noop() {
        let mut app = App::new(None);
        app.tick();
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn tick_recent_status_is_preserved() {
        let mut app = App::new(None);
        app.set_status("hello".into());
        app.tick();
        assert_eq!(app.status_msg(), Some("hello"));
    }

    #[test]
    fn app_set_port_updates_name_and_clears_selector() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        assert!(app.port_selector().is_some());
        app.set_port("COM1".into());
        assert_eq!(app.port_name(), Some("COM1"));
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn app_open_port_selector() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        let sel = app.port_selector().unwrap();
        assert_eq!(sel.ports(), &["COM1", "COM2"]);
    }

    #[test]
    fn refresh_port_selector_closes_on_empty() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        app.refresh_port_selector(vec![]);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn refresh_port_selector_updates_list_and_clamps_cursor() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        app.port_selector_mut().unwrap().move_cursor(1);
        app.refresh_port_selector(vec!["COM3".into()]);
        let sel = app.port_selector().unwrap();
        assert_eq!(sel.ports(), &["COM3"]);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn refresh_port_selector_no_op_when_closed() {
        let mut app = App::new(None);
        app.refresh_port_selector(vec!["COM1".into()]);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn push_line_adds_entry() {
        let mut app = App::new(None);
        app.push_line("I (1) wifi: Connected");
        assert_eq!(app.visible_entries(10).len(), 1);
    }

    #[test]
    fn push_line_records_tag() {
        let mut app = App::new(None);
        app.push_line("I (1) wifi: Connected");
        assert!(app.filter().known_tags().iter().any(|t| t == "wifi"));
    }

    #[test]
    fn push_line_blank_line_is_ignored() {
        let mut app = App::new(None);
        app.push_line("");
        app.push_line("   ");
        assert!(app.visible_entries(10).is_empty());
    }

    #[test]
    fn push_line_raw_line_does_not_record_tag() {
        let mut app = App::new(None);
        app.push_line("some raw output");
        assert!(app.filter().known_tags().is_empty());
    }

    #[test]
    fn push_line_scroll_increments_when_scrolled_up() {
        let mut app = App::new(None);
        app.push_line("I (1) tag: first");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        app.push_line("I (1) tag: second");
        assert_eq!(app.scroll(), 2);
    }

    #[test]
    fn push_line_scroll_stays_zero_at_bottom() {
        let mut app = App::new(None);
        app.push_line("I (1) tag: first");
        app.push_line("I (1) tag: second");
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn push_line_evicts_oldest_when_buffer_full() {
        let mut app = App::new(None);
        for i in 0..=BUFFER_SIZE {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        let entries = app.visible_entries(BUFFER_SIZE + 1);
        assert_eq!(entries.len(), BUFFER_SIZE);
        assert_eq!(entries[0].message(), "line 1");
        assert_eq!(
            entries[BUFFER_SIZE - 1].message(),
            &format!("line {BUFFER_SIZE}")
        );
    }

    #[test]
    fn visible_entries_empty_buffer() {
        let app = App::new(None);
        assert!(app.visible_entries(10).is_empty());
    }

    #[test]
    fn visible_entries_fewer_than_height_returns_all() {
        let mut app = App::new(None);
        for i in 0..3 {
            app.push_line(&format!("I (1) tag: line {i}"));
        }
        assert_eq!(app.visible_entries(10).len(), 3);
    }

    #[test]
    fn visible_entries_more_than_height_returns_tail() {
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
    fn visible_entries_respects_hidden_level() {
        let mut app = App::new(None);
        app.push_line("E (1) tag: error line");
        app.push_line("I (1) tag: info line");
        app.filter_mut().toggle_at_cursor();
        let entries = app.visible_entries(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message(), "info line");
    }

    #[test]
    fn handle_key_ctrl_c_quits() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn handle_key_q_quits() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn handle_key_q_exits_scroll_mode_when_scrolled() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::None);
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_esc_exits_scroll_mode_when_scrolled() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_esc_quits_when_not_scrolled() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Quit);
    }

    #[test]
    fn handle_key_d_disconnects() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('d'))), Action::Disconnect);
    }

    #[test]
    fn disconnect_clears_port_state() {
        let mut app = App::new(Some("COM1".into()));
        app.disconnect();
        assert!(app.port_name().is_none());
        assert!(app.port_cmd_tx().is_none());
    }

    #[test]
    fn handle_key_r_resets_device() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('r'))), Action::ResetDevice);
    }

    #[test]
    fn handle_key_c_scans_ports() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('c'))), Action::ScanPorts);
    }

    #[test]
    fn handle_key_f_returns_flash_action() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('f'))), Action::Flash);
    }

    #[test]
    fn handle_key_e_returns_erase_prompt_action() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('e'))), Action::ErasePrompt);
    }

    #[test]
    fn handle_key_s_is_noop() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('s'))), Action::None);
    }

    #[test]
    fn handle_key_tab_toggles_filter_popup() {
        let mut app = App::new(None);
        assert!(!app.filter().is_popup_open());
        app.handle_key(key(KeyCode::Tab));
        assert!(app.filter().is_popup_open());
        app.handle_key(key(KeyCode::Tab));
        assert!(!app.filter().is_popup_open());
    }

    #[test]
    fn handle_key_up_scrolls_up() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.scroll(), 1);
    }

    #[test]
    fn handle_key_down_scrolls_down_and_clamps() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.scroll(), 0);
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_page_up_adds_ten() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll(), 10);
    }

    #[test]
    fn handle_key_page_down_subtracts_ten() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::PageUp));
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_key_unknown_returns_none() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::F(1))), Action::None);
    }

    #[test]
    fn handle_key_filter_popup_space_toggles_item() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Tab));
        assert!(!app.filter().is_level_hidden(log::Level::Error));
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.filter().is_level_hidden(log::Level::Error));
    }

    #[test]
    fn handle_key_filter_popup_ctrl_a_toggles_all() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(ctrl(KeyCode::Char('a')));
        assert!(app.filter().is_level_hidden(log::Level::Error));
        assert!(app.filter().is_level_hidden(log::Level::Info));
    }

    #[test]
    fn handle_key_filter_popup_q_closes() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(!app.filter().is_popup_open());
    }

    #[test]
    fn handle_key_filter_popup_esc_closes() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.filter().is_popup_open());
    }

    #[test]
    fn handle_key_filter_popup_navigation() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.filter().cursor(), 1);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.filter().cursor(), 0);
    }

    #[test]
    fn handle_key_ctrl_c_quits_even_with_popup_open() {
        let mut app = App::new(None);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn handle_key_port_selector_navigation() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.port_selector().unwrap().cursor(), 1);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.port_selector().unwrap().cursor(), 0);
    }

    #[test]
    fn handle_key_port_selector_enter_returns_connect_action() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Action::ConnectPort("COM1".to_owned()));
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_port_selector_c_dismisses() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        let action = app.handle_key(key(KeyCode::Char('c')));
        assert_eq!(action, Action::None);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_port_selector_q_dismisses() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        let action = app.handle_key(key(KeyCode::Char('q')));
        assert_eq!(action, Action::None);
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_port_selector_esc_dismisses() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn handle_key_ctrl_c_quits_even_with_selector_open() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn push_line_scroll_no_drift_when_entry_filtered() {
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
    fn handle_key_ctrl_l_clears_log() {
        let mut app = App::new(None);
        app.push_line("I (1) tag: line");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('l'))), Action::None);
        assert!(app.visible_entries(10).is_empty());
        assert_eq!(app.scroll(), 0);
    }

    #[test]
    fn handle_ports_detected_no_op_when_empty_and_disconnected() {
        let mut app = App::new(None);
        handle_ports_detected(&mut app, vec![], &[], &make_tx());
        assert!(app.port_name().is_none());
        assert!(app.port_selector().is_none());
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn handle_ports_detected_opens_selector_for_multiple_ports() {
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        handle_ports_detected(&mut app, vec![], &["COM1".to_owned()], &make_tx());
        assert!(app.port_selector().is_none());
        assert_eq!(app.status_msg(), Some("No devices detected."));
    }

    #[tokio::test]
    async fn handle_ports_detected_auto_connects_when_selector_reaches_one_port() {
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
        let mut app = App::new(None);
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
    fn handle_ports_detected_is_noop_in_demo_mode() {
        let mut app = App::new(None);
        app.set_demo();
        handle_ports_detected(
            &mut app,
            vec!["/dev/ttyUSB0".into()],
            &[],
            &make_tx(),
        );
        assert!(
            app.port_name().is_none(),
            "PortsDetected must not connect a port in demo mode"
        );
        assert!(
            app.port_selector().is_none(),
            "PortsDetected must not open port selector in demo mode"
        );
        assert!(
            app.status_msg().is_none(),
            "PortsDetected must not set status in demo mode"
        );
    }

    #[test]
    fn handle_ports_detected_is_noop_while_flashing() {
        let mut app = App::new(None);
        app.set_flash_state(crate::flash::State::Flashing {
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
        let mut app = App::new(None);
        handle_action(&mut app, Action::Quit, &make_tx());
        assert!(!app.is_running());
    }

    #[test]
    fn handle_action_disconnect_when_connected() {
        let mut app = App::new(Some("COM1".into()));
        handle_action(&mut app, Action::Disconnect, &make_tx());
        assert!(app.port_name().is_none());
        assert_eq!(app.status_msg(), Some("Disconnected."));
    }

    #[test]
    fn handle_action_disconnect_when_not_connected() {
        let mut app = App::new(None);
        handle_action(&mut app, Action::Disconnect, &make_tx());
        assert_eq!(app.status_msg(), Some("Not connected."));
    }

    #[test]
    fn handle_action_reset_no_port() {
        let mut app = App::new(None);
        handle_action(&mut app, Action::ResetDevice, &make_tx());
        assert_eq!(app.status_msg(), Some("No port connected."));
    }

    #[test]
    fn handle_action_none_is_noop() {
        let mut app = App::new(None);
        handle_action(&mut app, Action::None, &make_tx());
        assert!(app.status_msg().is_none());
        assert!(app.is_running());
    }

    #[test]
    fn connect_success_commits_new_port_and_kills_old_source() {
        let (old_src_tx, _old_src_rx) = tokio::sync::watch::channel(false);
        let (new_src_tx, _new_src_rx) = tokio::sync::watch::channel(false);
        let (cmd_tx, _cmd_rx) = std::sync::mpsc::channel();

        let mut app = App::new(Some("COM1".into()));
        app.set_source_shutdown(old_src_tx);

        app.set_port("COM2".into());
        app.set_port_cmd(cmd_tx);
        app.set_source_shutdown(new_src_tx);
        app.set_status("Connected to COM2.".into());

        assert_eq!(app.port_name(), Some("COM2"));
        assert_eq!(app.status_msg(), Some("Connected to COM2."));
    }

    #[test]
    fn connect_error_preserves_existing_connection() {
        let mut app = App::new(Some("COM1".into()));
        let error_msg = "failed to open COM2: resource busy".to_owned();
        app.set_status(error_msg.clone());

        assert_eq!(
            app.port_name(),
            Some("COM1"),
            "existing port must survive a ConnectError"
        );
        assert_eq!(app.status_msg(), Some(error_msg.as_str()));
    }

    #[test]
    fn handle_action_scan_ports_leaves_app_in_consistent_state() {
        let mut app = App::new(None);
        handle_action(&mut app, Action::ScanPorts, &make_tx());
        assert!(
            app.status_msg().is_some()
                || app.port_name().is_some()
                || app.port_selector().is_some(),
            "scan_ports must produce an observable state change"
        );
    }

    #[test]
    fn demo_scan_ports_is_noop() {
        let mut app = App::new(None);
        app.set_demo();
        handle_action(&mut app, Action::ScanPorts, &make_tx());
        assert_eq!(
            app.status_msg(),
            Some("Not available in demo mode."),
            "ScanPorts must be blocked in demo mode"
        );
        assert!(app.port_name().is_none());
        assert!(app.port_selector().is_none());
    }

    #[test]
    fn demo_connect_port_is_noop() {
        let mut app = App::new(None);
        app.set_demo();
        app.set_port("demo".into());
        handle_action(
            &mut app,
            Action::ConnectPort("/dev/ttyUSB0".into()),
            &make_tx(),
        );
        assert_eq!(
            app.status_msg(),
            Some("Not available in demo mode."),
            "ConnectPort must be blocked in demo mode"
        );
        assert_eq!(app.port_name(), Some("demo"));
    }

    #[test]
    fn demo_reset_is_noop() {
        let mut app = App::new(None);
        app.set_demo();
        handle_action(&mut app, Action::ResetDevice, &make_tx());
        assert_eq!(app.status_msg(), Some("Not available in demo mode."));
    }

    #[test]
    fn handle_key_erase_confirm_y_confirms() {
        let mut app = App::new(None);
        app.open_erase_confirm();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('y'))),
            Action::ConfirmErase
        );
    }

    #[test]
    fn handle_key_erase_confirm_n_closes() {
        let mut app = App::new(None);
        app.open_erase_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Char('n'))), Action::None);
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_key_erase_confirm_esc_closes() {
        let mut app = App::new(None);
        app.open_erase_confirm();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_key_ctrl_c_quits_with_erase_confirm_open() {
        let mut app = App::new(None);
        app.open_erase_confirm();
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn handle_key_elf_selector_char_updates_input() {
        let mut app = App::new(None);
        app.open_elf_selector(None);
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.elf_selector().unwrap().value(), "/t");
    }

    #[test]
    fn handle_key_elf_selector_esc_closes() {
        let mut app = App::new(None);
        app.open_elf_selector(None);
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::CloseElfSelector);
    }

    #[test]
    fn handle_key_elf_selector_enter_confirms() {
        let mut app = App::new(None);
        app.open_elf_selector(None);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::ConfirmElfPath);
    }

    #[test]
    fn handle_action_flash_always_opens_selector() {
        let mut app = App::new(Some("COM1".into()));
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(app.is_elf_selector_open());
    }

    #[test]
    fn handle_action_confirm_elf_path_no_port_sets_status() {
        let path = std::env::temp_dir().join("esp-tui-test-elf-no-port");
        std::fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = App::new(None);
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector.as_mut() {
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
        let path = std::env::temp_dir().join("esp-tui-test-elf-flashing");
        std::fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Flashing {
            current: 0,
            total: 0,
        });
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector.as_mut() {
            for ch in path.to_str().unwrap().chars() {
                s.push_char(ch);
            }
        }
        handle_action(&mut app, Action::ConfirmElfPath, &make_tx());
        assert_eq!(app.status_msg(), Some("Flash already in progress."));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handle_action_flash_demo_is_noop() {
        let mut app = App::new(None);
        app.set_demo();
        handle_action(&mut app, Action::Flash, &make_tx());
        assert_eq!(app.status_msg(), Some("Not available in demo mode."));
    }

    #[test]
    fn handle_action_confirm_elf_path_valid() {
        let path = std::env::temp_dir().join("esp-tui-test-elf");
        std::fs::write(&path, b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = App::new(None);
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector.as_mut() {
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
        let mut app = App::new(None);
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector.as_mut() {
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
        let mut app = App::new(None);
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector.as_mut() {
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
        let path = std::env::temp_dir().join("esp-tui-test-non-elf");
        std::fs::write(&path, b"not an elf file").unwrap();
        let mut app = App::new(None);
        app.open_elf_selector(None);
        if let Some(s) = app.elf_selector.as_mut() {
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
        let mut app = App::new(None);
        assert!(!app.is_flashing());
        app.set_flash_state(crate::flash::State::Flashing {
            current: 0,
            total: 100,
        });
        assert!(app.is_flashing());
        app.set_flash_state(crate::flash::State::Erasing);
        assert!(app.is_flashing());
        app.set_flash_state(crate::flash::State::Idle);
        assert!(!app.is_flashing());
    }

    #[test]
    fn handle_action_erase_prompt_no_port_sets_status() {
        let mut app = App::new(None);
        handle_action(&mut app, Action::ErasePrompt, &make_tx());
        assert_eq!(app.status_msg(), Some("No port connected."));
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_action_erase_prompt_while_flashing_sets_status() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Flashing {
            current: 0,
            total: 0,
        });
        handle_action(&mut app, Action::ErasePrompt, &make_tx());
        assert_eq!(app.status_msg(), Some("Operation already in progress."));
        assert!(!app.is_erase_confirm_open());
    }

    #[test]
    fn handle_action_erase_prompt_connected_opens_confirm() {
        let mut app = App::new(Some("COM1".into()));
        handle_action(&mut app, Action::ErasePrompt, &make_tx());
        assert!(app.is_erase_confirm_open());
    }

    #[test]
    fn handle_action_flash_opens_selector_empty_when_no_elf() {
        let mut app = App::new(None);
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(app.is_elf_selector_open());
        assert_eq!(app.elf_selector().unwrap().value(), "");
    }

    #[test]
    fn handle_action_flash_opens_selector_prefilled_when_elf_set() {
        let mut app = App::new(None);
        app.set_elf_path(std::path::PathBuf::from("/tmp/firmware.elf"));
        handle_action(&mut app, Action::Flash, &make_tx());
        assert!(app.is_elf_selector_open());
        assert_eq!(app.elf_selector().unwrap().value(), "/tmp/firmware.elf");
    }

    #[test]
    fn handle_action_close_elf_selector_closes() {
        let mut app = App::new(None);
        app.open_elf_selector(None);
        handle_action(&mut app, Action::CloseElfSelector, &make_tx());
        assert!(!app.is_elf_selector_open());
    }

    #[test]
    fn demo_erase_is_noop() {
        let mut app = App::new(None);
        app.set_demo();
        handle_action(&mut app, Action::ErasePrompt, &make_tx());
        assert_eq!(app.status_msg(), Some("Not available in demo mode."));
    }

    #[test]
    fn demo_flash_is_noop() {
        let mut app = App::new(None);
        app.set_demo();
        handle_action(&mut app, Action::Flash, &make_tx());
        assert_eq!(app.status_msg(), Some("Not available in demo mode."));
    }
}

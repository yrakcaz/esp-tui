use std::collections::VecDeque;
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

use crate::{demo, event, filter, log, serial, ui};

const BUFFER_SIZE: usize = 10_000;
const STATUS_TTL_SECS: u64 = 3;

#[derive(Parser)]
#[command(name = "esp-tui", about = "ESP32 developer TUI")]
struct Args {
    /// Serial port to connect to.
    #[arg(long, short)]
    port: Option<String>,

    /// Run in demo mode with synthetic log output (no hardware required).
    #[arg(long)]
    demo: bool,
}

/// Outcome of a keypress that requires I/O, returned to the event loop to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    ResetDevice,
    Disconnect,
    ScanPorts,
    /// Connect to the given port name (emitted by the port selector popup).
    ConnectPort(String),
}

/// State for the port selection popup shown at startup when multiple ports are
/// detected.
pub struct PortSelector {
    ports: Vec<String>,
    cursor: usize,
}

impl PortSelector {
    /// Creates a new port selector with the given list of candidate ports.
    ///
    /// # Arguments
    ///
    /// * `ports` - Non-empty list of port names to select from.
    #[must_use]
    pub fn new(ports: Vec<String>) -> Self {
        Self { ports, cursor: 0 }
    }

    /// Returns all candidate port names.
    ///
    /// # Returns
    ///
    /// A slice of port name strings in selection order.
    #[must_use]
    pub fn ports(&self) -> &[String] {
        &self.ports
    }

    /// Returns the current cursor index.
    ///
    /// # Returns
    ///
    /// Zero-based index into the port list.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Moves the cursor by `delta`, clamped to the port list bounds.
    ///
    /// # Arguments
    ///
    /// * `delta` - Positive to move down, negative to move up.
    pub fn move_cursor(&mut self, delta: isize) {
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
    pub fn selected(&self) -> &str {
        self.ports.get(self.cursor).map_or("", String::as_str)
    }

    /// Replaces the candidate port list and clamps the cursor to the new
    /// bounds.
    ///
    /// # Arguments
    ///
    /// * `ports` - Updated list of available ports.
    pub fn update_ports(&mut self, ports: Vec<String>) {
        self.cursor = self.cursor.min(ports.len().saturating_sub(1));
        self.ports = ports;
    }
}

/// Central application state.
pub struct App {
    log_buffer: VecDeque<log::Entry>,
    scroll: usize,
    filter: filter::State,
    port_name: Option<String>,
    port_cmd_tx: Option<std::sync::mpsc::Sender<serial::PortCommand>>,
    source_shutdown_tx: Option<watch::Sender<bool>>,
    status_msg: Option<(String, Instant)>,
    running: bool,
    port_selector: Option<PortSelector>,
}

impl App {
    /// Creates a new application state.
    ///
    /// # Arguments
    ///
    /// * `port_name` - The connected serial port name, if already known.
    #[must_use]
    pub fn new(port_name: Option<String>) -> Self {
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
        }
    }

    /// Pushes a raw serial line into the log buffer, parsing it and evicting
    /// the oldest entry when the buffer is full.
    ///
    /// # Arguments
    ///
    /// * `line` - A single line of serial output.
    pub fn push_line(&mut self, line: &str) {
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

    /// Handles a keypress and returns the action the event loop should perform.
    ///
    /// # Arguments
    ///
    /// * `key` - The key event to handle.
    ///
    /// # Returns
    ///
    /// An [`Action`] indicating what I/O the event loop should perform.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            Action::Quit
        } else if self.port_selector.is_some() {
            match key.code {
                KeyCode::Up => {
                    if let Some(sel) = self.port_selector.as_mut() {
                        sel.move_cursor(-1);
                    }
                    Action::None
                }
                KeyCode::Down => {
                    if let Some(sel) = self.port_selector.as_mut() {
                        sel.move_cursor(1);
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
            }
        } else if self.filter.is_popup_open() {
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
            Action::None
        } else {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc if self.scroll > 0 => {
                    self.scroll = 0;
                    Action::None
                }
                KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
                KeyCode::Char('d') => Action::Disconnect,
                KeyCode::Char('r') => Action::ResetDevice,
                KeyCode::Char('f') => {
                    self.set_status("Flash: not implemented (Phase 2)".into());
                    Action::None
                }
                KeyCode::Char('e') => {
                    self.set_status("Erase: not implemented (Phase 2)".into());
                    Action::None
                }
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
    }

    /// Sets an ephemeral status message that expires after a few seconds.
    ///
    /// # Arguments
    ///
    /// * `msg` - The message to display in the status bar.
    pub fn set_status(&mut self, msg: String) {
        self.status_msg = Some((msg, Instant::now()));
    }

    /// Expires the status message if its TTL has elapsed. Called on each tick.
    pub fn tick(&mut self) {
        if let Some((_, ts)) = &self.status_msg {
            if ts.elapsed().as_secs() >= STATUS_TTL_SECS {
                self.status_msg = None;
            }
        }
    }

    /// Returns whether the application event loop should keep running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Returns the connected serial port name, if any.
    #[must_use]
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }

    /// Returns the current status message text, if any.
    #[must_use]
    pub fn status_msg(&self) -> Option<&str> {
        self.status_msg.as_ref().map(|(msg, _)| msg.as_str())
    }

    /// Returns a shared reference to the filter state.
    #[must_use]
    pub fn filter(&self) -> &filter::State {
        &self.filter
    }

    /// Returns a mutable reference to the filter state.
    ///
    /// # Returns
    ///
    /// A mutable reference to the current [`filter::State`].
    pub fn filter_mut(&mut self) -> &mut filter::State {
        &mut self.filter
    }

    /// Returns how many lines from the bottom are scrolled out of view.
    /// Zero means auto-scroll (pinned to the latest line).
    #[must_use]
    pub fn scroll(&self) -> usize {
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
    pub fn visible_entries(&self, height: usize) -> Vec<&log::Entry> {
        let visible: Vec<&log::Entry> = self
            .log_buffer
            .iter()
            .filter(|e| self.filter.is_visible(e))
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
    /// `Some` with a reference to the active [`PortSelector`], or `None` if no
    /// selector is open.
    #[must_use]
    pub fn port_selector(&self) -> Option<&PortSelector> {
        self.port_selector.as_ref()
    }

    /// Returns a mutable reference to the port selector, if active.
    ///
    /// # Returns
    ///
    /// `Some` with a mutable reference to the active [`PortSelector`], or
    /// `None` if no selector is open.
    pub fn port_selector_mut(&mut self) -> Option<&mut PortSelector> {
        self.port_selector.as_mut()
    }

    /// Sets the connected port name and clears the port selector.
    ///
    /// # Arguments
    ///
    /// * `port` - The port name to use going forward.
    pub fn set_port(&mut self, port: String) {
        self.port_name = Some(port);
        self.port_selector = None;
        self.port_cmd_tx = None;
    }

    /// Stores the command sender for the currently connected port reader task.
    ///
    /// # Arguments
    ///
    /// * `tx` - Sender returned by [`serial::Port::spawn`].
    pub fn set_port_cmd(
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
    pub fn port_cmd_tx(
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
    pub fn set_source_shutdown(&mut self, tx: watch::Sender<bool>) {
        if let Some(old) = self.source_shutdown_tx.replace(tx) {
            let _ = old.send(true);
        }
    }

    /// Stops the active data source, if any.
    pub fn shutdown_source(&mut self) {
        if let Some(tx) = self.source_shutdown_tx.take() {
            let _ = tx.send(true);
        }
    }

    /// Activates the port selector popup with the given candidate ports.
    ///
    /// # Arguments
    ///
    /// * `ports` - Non-empty list of port names to present for selection.
    pub fn open_port_selector(&mut self, ports: Vec<String>) {
        self.port_selector = Some(PortSelector::new(ports));
    }

    /// Updates the open port selector with a refreshed port list.
    ///
    /// Closes the selector when `ports` is empty; otherwise replaces the
    /// list and clamps the cursor.
    ///
    /// # Arguments
    ///
    /// * `ports` - Updated list of available ports.
    pub fn refresh_port_selector(&mut self, ports: Vec<String>) {
        if ports.is_empty() {
            self.port_selector = None;
        } else if let Some(sel) = self.port_selector.as_mut() {
            sel.update_ports(ports);
        }
    }

    /// Signals the event loop to stop.
    pub fn quit(&mut self) {
        self.running = false;
    }

    /// Clears the log buffer and resets the scroll offset to zero.
    pub fn clear_log(&mut self) {
        self.log_buffer.clear();
        self.scroll = 0;
    }

    /// Tears down the active port connection and clears port state.
    pub fn disconnect(&mut self) {
        self.shutdown_source();
        self.port_name = None;
        self.port_cmd_tx = None;
    }
}

/// Runs the application: parses CLI arguments, initialises the terminal, and
/// drives the event loop until the user quits.
///
/// # Errors
///
/// Returns an error if terminal initialisation or any I/O operation fails.
pub async fn run() -> anyhow::Result<()> {
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

async fn run_inner(args: Args) -> anyhow::Result<()> {
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal =
        Terminal::new(backend).context("failed to create terminal")?;

    let (tx, mut rx) = mpsc::unbounded_channel::<event::Message>();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut app = App::new(None);

    if args.demo {
        let (src_tx, src_rx) = watch::channel(false);
        let _ = demo::Generator.spawn(tx.clone(), src_rx);
        app.set_port("demo".into());
        app.set_source_shutdown(src_tx);
        app.set_status("Connected to demo.".into());
    } else {
        let mut ports = resolve_ports(args.port)?;
        match ports.len() {
            0 => {}
            1 => connect_port(&mut app, ports.remove(0), &tx),
            _ => app.open_port_selector(ports),
        }
    }
    spawn_port_poller(tx.clone(), shutdown_rx.clone());

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
            event::Message::Tick => app.tick(),
            event::Message::PortsDetected(ports) => {
                handle_ports_detected(&mut app, ports, &tx);
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

fn handle_action(
    app: &mut App,
    action: Action,
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    match action {
        Action::Quit => app.quit(),
        Action::Disconnect => {
            if app.port_name().is_some() {
                app.disconnect();
                app.set_status("Disconnected.".into());
            } else {
                app.set_status("Not connected.".into());
            }
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
        Action::ConnectPort(port) => connect_port(app, port, tx),
        Action::None => {}
    }
}

fn connect_port(
    app: &mut App,
    port: String,
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    let (src_tx, src_rx) = watch::channel(false);
    let status = format!("Connected to {port}.");
    let (_, cmd_tx) = serial::Port::new(&port).spawn(tx.clone(), src_rx);
    app.set_port(port);
    app.set_port_cmd(cmd_tx);
    app.set_source_shutdown(src_tx);
    app.set_status(status);
}

fn apply_scan(app: &mut App, tx: &mpsc::UnboundedSender<event::Message>) {
    match serial::detect_esp_ports() {
        Err(e) => app.set_status(format!("Port scan failed: {e}")),
        Ok(ports) if ports.is_empty() => {
            app.set_status("No devices detected.".into());
        }
        Ok(mut ports) if ports.len() == 1 => {
            connect_port(app, ports.remove(0), tx);
        }
        Ok(ports) => app.open_port_selector(ports),
    }
}

fn resolve_ports(port_arg: Option<String>) -> anyhow::Result<Vec<String>> {
    port_arg.map_or_else(serial::detect_esp_ports, |p| Ok(vec![p]))
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
                            last_ports.clone_from(&ports);
                            if tx.send(event::Message::PortsDetected(ports)).is_err() {
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

fn handle_ports_detected(
    app: &mut App,
    mut ports: Vec<String>,
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    if app.port_name().is_none() {
        if app.port_selector().is_some() {
            app.refresh_port_selector(ports);
            if app.port_selector().is_none() {
                app.set_status("No devices detected.".into());
            }
        } else {
            match ports.len() {
                0 => {}
                1 => connect_port(app, ports.remove(0), tx),
                _ => app.open_port_selector(ports),
            }
        }
    } else if let Some(current) = app.port_name() {
        let current_present = ports.iter().any(|p| p.as_str() == current);
        if ports.len() > 1 && current_present {
            app.set_status("New device detected. Press [c] to connect.".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use tokio::sync::mpsc;

    use super::{
        handle_action, handle_ports_detected, Action, App, PortSelector, BUFFER_SIZE,
    };
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
    fn port_selector_initial_cursor() {
        let sel = PortSelector::new(vec!["COM1".into(), "COM2".into()]);
        assert_eq!(sel.cursor(), 0);
        assert_eq!(sel.selected(), "COM1");
    }

    #[test]
    fn port_selector_move_cursor_navigation() {
        let mut sel =
            PortSelector::new(vec!["COM1".into(), "COM2".into(), "COM3".into()]);
        sel.move_cursor(1);
        assert_eq!(sel.cursor(), 1);
        assert_eq!(sel.selected(), "COM2");
        sel.move_cursor(-1);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn port_selector_move_cursor_clamps() {
        let mut sel = PortSelector::new(vec!["COM1".into(), "COM2".into()]);
        sel.move_cursor(-10);
        assert_eq!(sel.cursor(), 0);
        sel.move_cursor(100);
        assert_eq!(sel.cursor(), 1);
    }

    #[test]
    fn port_selector_move_cursor_empty_list() {
        let mut sel = PortSelector::new(vec![]);
        sel.move_cursor(1);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn port_selector_selected_empty() {
        let sel = PortSelector::new(vec![]);
        assert_eq!(sel.selected(), "");
    }

    #[test]
    fn port_selector_update_ports_replaces_list_and_clamps_cursor() {
        let mut sel =
            PortSelector::new(vec!["COM1".into(), "COM2".into(), "COM3".into()]);
        sel.move_cursor(2);
        sel.update_ports(vec!["COM4".into()]);
        assert_eq!(sel.ports(), &["COM4"]);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn port_selector_update_ports_empty_resets_cursor() {
        let mut sel = PortSelector::new(vec!["COM1".into()]);
        sel.update_ports(vec![]);
        assert_eq!(sel.cursor(), 0);
        assert!(sel.ports().is_empty());
    }

    #[test]
    fn app_initial_state() {
        let app = App::new(Some("COM1".into()));
        assert!(app.is_running());
        assert_eq!(app.port_name(), Some("COM1"));
        assert_eq!(app.scroll(), 0);
        assert!(app.status_msg().is_none());
        assert!(app.port_selector().is_none());
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
    fn handle_key_f_sets_status_and_returns_none() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('f'))), Action::None);
        assert!(app.status_msg().is_some());
    }

    #[test]
    fn handle_key_e_sets_status_and_returns_none() {
        let mut app = App::new(None);
        assert_eq!(app.handle_key(key(KeyCode::Char('e'))), Action::None);
        assert!(app.status_msg().is_some());
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
        app.filter_mut().move_cursor(2); // Info is at index 2
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
        handle_ports_detected(&mut app, vec![], &make_tx());
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
            &make_tx(),
        );
        let sel = app.port_selector().unwrap();
        assert_eq!(sel.ports(), &["COM3", "COM4"]);
    }

    #[test]
    fn handle_ports_detected_closes_selector_on_empty() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into()]);
        handle_ports_detected(&mut app, vec![], &make_tx());
        assert!(app.port_selector().is_none());
        assert_eq!(app.status_msg(), Some("No devices detected."));
    }

    #[test]
    fn handle_ports_detected_connected_new_device_sets_status() {
        let mut app = App::new(None);
        app.set_port("COM1".into());
        handle_ports_detected(
            &mut app,
            vec!["COM1".into(), "COM2".into()],
            &make_tx(),
        );
        assert!(app.status_msg().is_some());
    }

    #[test]
    fn handle_ports_detected_connected_same_ports_no_status() {
        let mut app = App::new(None);
        app.set_port("COM1".into());
        handle_ports_detected(&mut app, vec!["COM1".into()], &make_tx());
        assert!(app.status_msg().is_none());
    }

    #[test]
    fn handle_ports_detected_connected_current_gone_no_new_device_status() {
        let mut app = App::new(None);
        app.set_port("COM1".into());
        handle_ports_detected(&mut app, vec!["COM2".into()], &make_tx());
        assert!(app.status_msg().is_none());
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
    fn handle_action_scan_ports_does_not_panic() {
        let mut app = App::new(None);
        handle_action(&mut app, Action::ScanPorts, &make_tx());
        // Outcome depends on available ports; verifies no panic.
    }
}

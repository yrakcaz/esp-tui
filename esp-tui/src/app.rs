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

use crate::source::Emitter;
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
    #[must_use]
    pub fn ports(&self) -> &[String] {
        &self.ports
    }

    /// Returns the current cursor index.
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
        if len == 0 {
            return;
        }
        self.cursor = self.cursor.saturating_add_signed(delta).min(len - 1);
    }

    /// Returns the currently selected port name.
    #[must_use]
    pub fn selected(&self) -> &str {
        self.ports.get(self.cursor).map_or("", String::as_str)
    }
}

/// Central application state.
pub struct App {
    log_buffer: VecDeque<log::Entry>,
    scroll: usize,
    filter: filter::State,
    port_name: Option<String>,
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
        if self.scroll > 0 {
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
            return Action::Quit;
        }

        if let Some(sel) = self.port_selector.as_mut() {
            match key.code {
                KeyCode::Up => sel.move_cursor(-1),
                KeyCode::Down => sel.move_cursor(1),
                KeyCode::Enter => {
                    let port = sel.selected().to_owned();
                    self.port_selector = None;
                    return Action::ConnectPort(port);
                }
                KeyCode::Char('q') => return Action::Quit,
                _ => {}
            }
            return Action::None;
        }

        if self.filter.is_popup_open() {
            match key.code {
                KeyCode::Up => self.filter.move_cursor(-1),
                KeyCode::Down => self.filter.move_cursor(1),
                KeyCode::Char(' ') => self.filter.toggle_at_cursor(),
                KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
                    self.filter.clear_hidden();
                }
                KeyCode::Tab | KeyCode::Esc => self.filter.toggle_popup(),
                _ => {}
            }
            return Action::None;
        }

        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('r') => Action::ResetDevice,
            KeyCode::Char('f') => {
                self.set_status("Flash: not implemented (Phase 2)".into());
                Action::None
            }
            KeyCode::Char('e') => {
                self.set_status("Erase: not implemented (Phase 2)".into());
                Action::None
            }
            KeyCode::Char('c') => {
                self.set_status("Connect: not implemented (Phase 2)".into());
                Action::None
            }
            KeyCode::Tab => {
                self.filter.toggle_popup();
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_add(1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
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
        let filtered: Vec<&log::Entry> = self
            .log_buffer
            .iter()
            .filter(|e| self.filter.is_visible(e))
            .collect();
        let total = filtered.len();
        let skip = self.scroll.min(total.saturating_sub(height)).min(total);
        let start = total.saturating_sub(height).saturating_sub(skip);
        filtered.get(start..).unwrap_or_default().to_vec()
    }

    /// Returns a shared reference to the port selector, if active.
    #[must_use]
    pub fn port_selector(&self) -> Option<&PortSelector> {
        self.port_selector.as_ref()
    }

    /// Returns a mutable reference to the port selector, if active.
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
    }

    /// Activates the port selector popup with the given candidate ports.
    ///
    /// # Arguments
    ///
    /// * `ports` - Non-empty list of port names to present for selection.
    pub fn open_port_selector(&mut self, ports: Vec<String>) {
        self.port_selector = Some(PortSelector::new(ports));
    }

    /// Signals the event loop to stop.
    pub fn quit(&mut self) {
        self.running = false;
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

    let mut pending_ports: Option<Vec<String>> = None;

    let initial_port = if args.demo {
        demo::Generator.spawn(tx.clone(), shutdown_rx.clone());
        None
    } else {
        let ports = resolve_ports(args.port)?;
        if ports.len() == 1 {
            let port = ports.into_iter().next().unwrap();
            serial::Port::new(port.clone()).spawn(tx.clone(), shutdown_rx.clone());
            Some(port)
        } else {
            pending_ports = Some(ports);
            None
        }
    };

    let mut app = App::new(initial_port);
    if let Some(ports) = pending_ports {
        app.open_port_selector(ports);
    }
    let mut tick = interval(Duration::from_millis(250));

    let key_tx = tx.clone();
    tokio::task::spawn_blocking(move || loop {
        match crossterm::event::read() {
            Ok(Event::Key(key)) => {
                if key_tx.send(event::Message::Key(key)).is_err() {
                    break;
                }
            }
            Ok(_) => {}
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
                match action {
                    Action::Quit => {
                        app.quit();
                    }
                    Action::ResetDevice => {
                        if let Some(port) = app.port_name() {
                            if let Err(e) = serial::reset_device(port) {
                                app.set_status(format!("Reset failed: {e}"));
                            } else {
                                app.set_status("Reset sent.".into());
                            }
                        } else {
                            app.set_status("No port connected.".into());
                        }
                    }
                    Action::ConnectPort(port) => {
                        app.set_port(port.clone());
                        serial::Port::new(port)
                            .spawn(tx.clone(), shutdown_rx.clone());
                    }
                    Action::None => {}
                }
            }
            event::Message::Serial(line) => app.push_line(&line),
            event::Message::Tick => app.tick(),
        }

        if !app.is_running() {
            break;
        }
    }

    let _ = shutdown_tx.send(true);
    Ok(())
}

fn resolve_ports(port_arg: Option<String>) -> anyhow::Result<Vec<String>> {
    if let Some(p) = port_arg {
        return Ok(vec![p]);
    }
    let ports = serial::detect_esp_ports()?;
    if ports.is_empty() {
        anyhow::bail!("no serial ports found; use --port or --demo");
    }
    Ok(ports)
}

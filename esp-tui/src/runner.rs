use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use crossterm::event::Event;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, Duration};

use crate::app::{Action, App, DEFAULT_BAUD};
use crate::{elf, event, flash, serial, ui};

#[derive(Parser)]
#[command(name = "esp-tui", about = "ESP32 developer TUI")]
struct Args {
    /// Serial port to connect to.
    #[arg(long, short)]
    port: Option<String>,

    /// Serial baud rate.
    #[arg(long, short = 'b')]
    baud: Option<u32>,
}

fn begin_connect(port: &str, baud: u32, tx: &mpsc::UnboundedSender<event::Message>) {
    let (src_tx, src_rx) = watch::channel(false);
    let port_name = port.to_owned();
    let tx_task = tx.clone();
    let _handle = tokio::task::spawn_blocking(move || {
        serial::Port::new(&port_name, baud)
            .connect_and_read(&tx_task, &src_rx, src_tx);
    });
}

// After flash/erase the chip is already reset by espflash and device info was
// collected during the operation. Probing again would re-enter the ROM
// bootloader, so this function skips the probe and goes straight to the
// firmware-mode reset and serial reader.
fn begin_reconnect(
    port: &str,
    baud: u32,
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    let (src_tx, src_rx) = watch::channel(false);
    let port_name = port.to_owned();
    let tx_task = tx.clone();
    let _handle = tokio::task::spawn_blocking(move || {
        // Wait for the chip to finish its espflash-initiated reset and for any
        // USB re-enumeration to settle before asserting our own reset.
        std::thread::sleep(std::time::Duration::from_millis(500));
        serial::reset_to_run(&port_name, baud);
        std::thread::sleep(std::time::Duration::from_millis(500));
        serial::Port::new(&port_name, baud)
            .connect_and_read(&tx_task, &src_rx, src_tx);
    });
}

fn resolve_ports(port_arg: Option<String>) -> anyhow::Result<Vec<String>> {
    port_arg.map_or_else(serial::detect_esp_ports, |p| Ok(vec![p]))
}

fn apply_scan(app: &mut App, tx: &mpsc::UnboundedSender<event::Message>) {
    if app.is_flashing() {
        app.set_status("Operation already in progress.");
    } else {
        match serial::detect_esp_ports() {
            Err(e) => app.set_status(format!("Port scan failed: {e}")),
            Ok(ports) if ports.is_empty() => {
                app.set_status("No devices detected.");
            }
            Ok(mut ports) if ports.len() == 1 => {
                let port = ports.remove(0);
                app.set_status(format!("Connecting to {port}..."));
                begin_connect(&port, app.baud(), tx);
                app.set_port(port);
            }
            Ok(ports) => app.open_port_selector(ports),
        }
    }
}

/// Reacts to a change in the detected serial port set.
///
/// Connects automatically when exactly one port is available and no port is
/// already selected; refreshes an open port-selector popup otherwise.
///
/// # Arguments
///
/// * `app` - Mutable reference to application state.
/// * `current` - The newly detected port list.
/// * `previous` - The port list from the previous poll.
/// * `tx` - Sender for the event channel; used to initiate connections.
pub(crate) fn handle_ports_detected(
    app: &mut App,
    mut current: Vec<String>,
    previous: &[String],
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    if !app.is_flashing() {
        if app.port_name().is_none() {
            if app.port_selector().is_some() {
                match current.len() {
                    0 => {
                        app.close_port_selector();
                        app.set_status("No devices detected.");
                    }
                    1 => {
                        app.close_port_selector();
                        let port = current.remove(0);
                        app.set_status(format!("Connecting to {port}..."));
                        begin_connect(&port, app.baud(), tx);
                        app.set_port(port);
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
                        app.set_port(port);
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
                app.set_status("New device detected. Press [c] to connect.");
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
                            let previous =
                                std::mem::replace(&mut last_ports, ports.clone());
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
        app.set_status("Path is a directory.");
    } else if !path.is_file() {
        app.set_status("Path not found.");
    } else if !elf::is_elf_file(&path) {
        app.set_status("Not a valid ELF file.");
    } else {
        app.set_elf_path(path);
        app.close_elf_selector();
        do_flash(app, tx);
    }
}

fn start_flash(app: &mut App) {
    if app.port_name().is_none() {
        app.set_status("No port connected.");
    } else if app.is_flashing() {
        app.set_status("Operation already in progress.");
    } else {
        let prefill = app.elf_path().map(Path::to_path_buf);
        app.open_elf_selector(prefill.as_deref());
    }
}

// The serial reader has a 100ms read timeout; the 200ms delay lets it observe
// the shutdown signal and release the port fd before the operation begins.
fn spawn_hardware_op<F>(
    app: &mut App,
    state: flash::State,
    tx: &mpsc::UnboundedSender<event::Message>,
    op: F,
) where
    F: FnOnce() -> event::Message + Send + 'static,
{
    app.shutdown_source();
    app.set_flash_state(state);
    let tx_task = tx.clone();
    let _ = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = tx_task.send(op());
    });
}

fn do_flash(app: &mut App, tx: &mpsc::UnboundedSender<event::Message>) {
    if app.is_flashing() {
        app.set_status("Flash already in progress.");
    } else {
        match (
            app.port_name().map(str::to_owned),
            app.elf_path().map(Path::to_path_buf),
        ) {
            (None, _) => app.set_status("No port connected."),
            (_, None) => {}
            (Some(port), Some(elf_path)) => {
                let baud = app.baud();
                let tx_progress = tx.clone();
                spawn_hardware_op(
                    app,
                    flash::State::Flashing {
                        addr: 0,
                        current: 0,
                        total: 0,
                    },
                    tx,
                    move || {
                        event::Message::FlashDone(flash::flash_elf(
                            &port,
                            baud,
                            &elf_path,
                            tx_progress,
                        ))
                    },
                );
            }
        }
    }
}

/// Dispatches an [`Action`] to the appropriate app state mutation or I/O
/// side-effect.
///
/// # Arguments
///
/// * `app` - Mutable reference to application state.
/// * `action` - The action to dispatch.
/// * `tx` - Sender for the event channel; used to queue I/O results.
pub(crate) fn handle_action(
    app: &mut App,
    action: Action,
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    match action {
        Action::None => {}
        Action::Quit => app.quit(),
        Action::QuitPrompt => {
            if app.is_flashing() {
                app.set_status("Operation already in progress.");
            } else {
                app.open_quit_confirm();
            }
        }
        Action::Disconnect => {
            if app.is_flashing() {
                app.set_status("Operation already in progress.");
            } else if app.port_name().is_some() {
                app.disconnect();
                app.set_status("Disconnected.");
            } else {
                app.set_status("Not connected.");
            }
        }
        Action::CloseElfSelector => app.close_elf_selector(),
        Action::ConfirmElfPath => confirm_elf_path(app, tx),
        Action::ResetDevice => {
            if app.is_flashing() {
                app.set_status("Operation already in progress.");
            } else {
                match app.port_cmd_tx() {
                    Some(cmd_tx) => {
                        if cmd_tx.send(serial::PortCommand::Reset).is_err() {
                            app.set_status("Reset failed: port disconnected.");
                        } else {
                            app.set_status("Reset sent.");
                        }
                    }
                    None if app.port_name().is_some() => {
                        app.set_status("Reset not supported.");
                    }
                    None => app.set_status("No port connected."),
                }
            }
        }
        Action::ScanPorts => apply_scan(app, tx),
        Action::ConnectPort(port) => {
            if app.is_flashing() {
                app.set_status("Operation already in progress.");
            } else {
                app.set_status(format!("Connecting to {port}..."));
                begin_connect(&port, app.baud(), tx);
                app.set_port(port);
            }
        }
        Action::ErasePrompt => {
            if app.port_name().is_none() {
                app.set_status("No port connected.");
            } else if app.is_flashing() {
                app.set_status("Operation already in progress.");
            } else {
                app.open_erase_confirm();
            }
        }
        Action::ConfirmErase => {
            app.close_erase_confirm();
            if let Some(port) = app.port_name().map(str::to_owned) {
                let baud = app.baud();
                spawn_hardware_op(app, flash::State::Erasing, tx, move || {
                    event::Message::EraseDone(flash::erase_flash(&port, baud))
                });
            }
        }
        Action::Flash => start_flash(app),
    }
}

/// Processes a single [`event::Message`] from the event channel, updating
/// application state and triggering any necessary I/O.
///
/// # Arguments
///
/// * `app` - Mutable reference to application state.
/// * `msg` - The event message to handle.
/// * `baud` - Baud rate used when reconnecting after flash or erase.
/// * `tx` - Sender for the event channel; used to queue follow-up messages.
pub(crate) fn handle_event_message(
    app: &mut App,
    msg: event::Message,
    baud: u32,
    tx: &mpsc::UnboundedSender<event::Message>,
) {
    match msg {
        event::Message::Key(key) => {
            let action = app.handle_key(key);
            handle_action(app, action, tx);
        }
        event::Message::Serial(line) => app.push_line(&line),
        event::Message::Disconnected => {
            app.disconnect();
            app.set_status("Disconnected.");
        }
        event::Message::ConnectSuccess {
            port,
            cmd_tx,
            src_tx,
        } => {
            let status = format!("Connected to {port}.");
            app.clear_agent_data();
            app.set_port(port);
            app.set_port_cmd(cmd_tx);
            app.set_source_shutdown(src_tx);
            app.set_flash_state(flash::State::Idle);
            app.set_status(status);
        }
        event::Message::ConnectError(msg) => {
            app.set_status(msg);
            app.disconnect();
            app.set_flash_state(flash::State::Idle);
        }
        event::Message::Tick => app.tick(),
        event::Message::PortsDetected { current, previous } => {
            handle_ports_detected(app, current, &previous, tx);
        }
        event::Message::FlashProgress {
            addr,
            current,
            total,
        } => {
            app.set_flash_state(flash::State::Flashing {
                addr,
                current,
                total,
            });
        }
        event::Message::FlashDone(result) => {
            match result {
                Ok(()) => {
                    app.set_status("Flash complete. Reconnecting...");
                }
                Err(e) => {
                    app.set_status(format!("Flash failed: {e}"));
                }
            }
            app.set_flash_state(flash::State::Reconnecting);
            if let Some(port) = app.port_name().map(str::to_owned) {
                begin_reconnect(&port, baud, tx);
            }
        }
        event::Message::DeviceInfo(result) => {
            if let Ok(info) = result {
                app.set_device_info(info);
            }
        }
        event::Message::EraseDone(result) => {
            match result {
                Ok(()) => {
                    app.set_status("Erase complete.");
                }
                Err(e) => {
                    app.set_status(format!("Erase failed: {e}"));
                }
            }
            // The device was disconnected before the erase; reconnect is always
            // attempted to restore the serial link, same as after flash.
            app.set_flash_state(flash::State::Reconnecting);
            if let Some(port) = app.port_name().map(str::to_owned) {
                begin_reconnect(&port, baud, tx);
            }
        }
    }
}

async fn run_inner(args: Args) -> anyhow::Result<()> {
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal =
        Terminal::new(backend).context("failed to create terminal")?;

    let (tx, mut rx) = mpsc::unbounded_channel::<event::Message>();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let baud = args.baud.unwrap_or(DEFAULT_BAUD);
    let mut app = App::new(None);
    app.set_baud(baud);

    let mut ports = resolve_ports(args.port)?;
    match ports.len() {
        0 => {}
        1 => {
            let port = ports.remove(0);
            app.set_status(format!("Connecting to {port}..."));
            begin_connect(&port, baud, &tx);
            app.set_port(port);
        }
        _ => app.open_port_selector(ports),
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

        handle_event_message(&mut app, msg, baud, &tx);

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

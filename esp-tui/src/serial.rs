use std::io::BufRead;

use anyhow::Context;
use serialport::SerialPortInfo;

const BAUD_RATE: u32 = 115_200;

const ESP_USB_VIDS: &[u16] = &[
    0x10C4, // Silicon Labs CP210x
    0x0403, // FTDI
    0x303A, // Espressif native USB
];

fn is_esp_port(info: &SerialPortInfo) -> bool {
    match &info.port_type {
        serialport::SerialPortType::UsbPort(usb) => ESP_USB_VIDS.contains(&usb.vid),
        _ => false,
    }
}

/// Detects available ESP32 serial ports by filtering system USB serial ports
/// against known ESP32 USB vendor IDs.
///
/// Falls back to all USB serial ports if none match the known VIDs.
///
/// # Returns
///
/// A list of port name strings. May be empty if no USB serial ports are found.
///
/// # Errors
///
/// Returns an error if the system port enumeration fails.
pub fn detect_esp_ports() -> anyhow::Result<Vec<String>> {
    let all =
        serialport::available_ports().context("failed to enumerate serial ports")?;

    let esp: Vec<String> = all
        .iter()
        .filter(|p| is_esp_port(p))
        .map(|p| p.port_name.clone())
        .collect();

    if esp.is_empty() {
        Ok(all
            .iter()
            .filter(|p| {
                matches!(p.port_type, serialport::SerialPortType::UsbPort(_))
            })
            .map(|p| p.port_name.clone())
            .collect())
    } else {
        Ok(esp)
    }
}

/// A serial port connection that emits log lines.
pub struct Port {
    name: String,
}

impl Port {
    /// Creates a new serial port source for the given port name.
    ///
    /// # Arguments
    ///
    /// * `name` - The system port name (e.g. `/dev/ttyUSB0`).
    #[must_use]
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

/// Sends a hardware reset to the device on the given port by toggling DTR and
/// RTS lines.
///
/// # Arguments
///
/// * `port_name` - The system port name to open for the reset sequence.
///
/// # Errors
///
/// Returns an error if the port cannot be opened or the control lines cannot
/// be set.
pub fn reset_device(port_name: &str) -> anyhow::Result<()> {
    let mut port = serialport::new(port_name, BAUD_RATE)
        .open()
        .with_context(|| format!("failed to open {port_name} for reset"))?;
    port.write_data_terminal_ready(false)
        .context("failed to set DTR")?;
    port.write_request_to_send(true)
        .context("failed to set RTS")?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    port.write_data_terminal_ready(true)
        .context("failed to release DTR")?;
    port.write_request_to_send(false)
        .context("failed to release RTS")?;
    Ok(())
}

impl crate::source::Emitter for Port {
    /// Spawns a blocking task that reads lines from the serial port and sends
    /// them as [`crate::event::Message::Serial`] events.
    ///
    /// # Arguments
    ///
    /// * `tx` - Channel sender for forwarding log line events.
    /// * `shutdown` - Watch receiver; the task exits when the value becomes
    ///   `true`.
    fn spawn(
        self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::event::Message>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            let port = match serialport::new(&self.name, BAUD_RATE)
                .timeout(std::time::Duration::from_millis(100))
                .open()
            {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.send(crate::event::Message::Serial(format!(
                        "failed to open {}: {e}",
                        self.name
                    )));
                    return;
                }
            };

            let mut reader = std::io::BufReader::new(port);
            let mut line = String::new();

            loop {
                if *shutdown.borrow() {
                    break;
                }
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end().to_owned();
                        line.clear();
                        if tx.send(crate::event::Message::Serial(trimmed)).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
            }
        })
    }
}

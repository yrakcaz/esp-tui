use std::io::BufRead;
use std::sync::mpsc;

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

/// Commands that can be sent to a running serial port reader task.
pub enum PortCommand {
    Reset,
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
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Spawns a blocking task that reads lines from the serial port and sends
    /// them as [`crate::event::Message::Serial`] events.
    ///
    /// Returns a [`PortCommand`] sender that can be used to send commands
    /// (e.g. reset) to the running task without opening a second handle.
    ///
    /// # Arguments
    ///
    /// * `tx` - Channel sender for forwarding log line events.
    /// * `shutdown` - Watch receiver; the task exits when the value becomes
    ///   `true`.
    ///
    /// # Returns
    ///
    /// A tuple of the task [`tokio::task::JoinHandle`] and a
    /// [`std::sync::mpsc::Sender`] for sending [`PortCommand`]s.
    #[must_use]
    pub fn spawn(
        self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::event::Message>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> (tokio::task::JoinHandle<()>, mpsc::Sender<PortCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<PortCommand>();
        let handle = tokio::task::spawn_blocking(move || {
            match serialport::new(&self.name, BAUD_RATE)
                .timeout(std::time::Duration::from_millis(100))
                .open()
            {
                Err(e) => {
                    let _ = tx.send(crate::event::Message::Serial(format!(
                        "failed to open {}: {e}",
                        self.name
                    )));
                    let _ = tx.send(crate::event::Message::Disconnected);
                }
                Ok(port) => {
                    let mut reader = std::io::BufReader::new(port);
                    let mut line = String::new();

                    loop {
                        if *shutdown.borrow() {
                            break;
                        }
                        if let Ok(PortCommand::Reset) = cmd_rx.try_recv() {
                            if let Err(e) = reset_via_handle(reader.get_mut()) {
                                let _ = tx.send(crate::event::Message::Serial(
                                    format!("Reset failed: {e}"),
                                ));
                            }
                        }
                        match reader.read_line(&mut line) {
                            Ok(0) => {
                                let _ = tx.send(crate::event::Message::Disconnected);
                                break;
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end().to_owned();
                                line.clear();
                                if tx
                                    .send(crate::event::Message::Serial(trimmed))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                            Err(_) => {
                                let _ = tx.send(crate::event::Message::Disconnected);
                                break;
                            }
                        }
                    }
                }
            }
        });
        (handle, cmd_tx)
    }
}

fn reset_via_handle(
    port: &mut Box<dyn serialport::SerialPort>,
) -> anyhow::Result<()> {
    port.write_data_terminal_ready(false)
        .context("failed to set DTR")?;
    port.write_request_to_send(true)
        .context("failed to assert RTS/EN")?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    port.write_request_to_send(false)
        .context("failed to release RTS/EN")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

    use super::*;

    fn usb_port(vid: u16) -> SerialPortInfo {
        SerialPortInfo {
            port_name: "/dev/ttyUSB0".into(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 0x0001,
                serial_number: None,
                manufacturer: None,
                product: None,
            }),
        }
    }

    #[test]
    fn is_esp_port_matches_known_vids() {
        assert!(is_esp_port(&usb_port(0x10C4)));
        assert!(is_esp_port(&usb_port(0x0403)));
        assert!(is_esp_port(&usb_port(0x303A)));
    }

    #[test]
    fn is_esp_port_rejects_unknown_vid() {
        assert!(!is_esp_port(&usb_port(0xBEEF)));
    }

    #[test]
    fn is_esp_port_rejects_non_usb() {
        let info = SerialPortInfo {
            port_name: "/dev/ttyS0".into(),
            port_type: SerialPortType::Unknown,
        };
        assert!(!is_esp_port(&info));
    }
}

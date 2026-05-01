use tokio::time::{interval, Duration};

static LINES: &[&str] = &[
    "I (1000) wifi: Connected to AP \"DemoNetwork\"",
    "I (1500) nvs: Reading key \"boot_count\" = 7",
    "W (2000) heap: Stack usage near limit: 91%",
    "I (2500) app: Entering main loop",
    "D (3000) gpio: Pin 2 set HIGH",
    "E (3500) i2c: Timeout waiting for ACK on addr 0x3C",
    "I (4000) wifi: RSSI = -62 dBm",
    "V (4500) spi: Transfer complete, 64 bytes",
    "W (5000) app: Retry count = 3",
    "I (5500) ota: Checking for firmware update",
    "D (6000) nvs: Writing key \"last_seen\" = 1700000000",
    "E (6500) uart: RX buffer overflow",
    "I (7000) app: Free heap: 142 KB",
    "V (7500) timer: Tick 42",
    "I (8000) wifi: Disconnected, reconnecting...",
];

/// Spawns an async task that cycles through pre-defined ESP-IDF log lines
/// every 100ms, for UI development without hardware.
///
/// # Arguments
///
/// * `tx` - Sender for forwarding demo lines as
///   [`crate::event::Message::Serial`] events.
/// * `shutdown` - Watch receiver; the task exits when the value becomes
///   `true`.
pub(crate) fn spawn(
    tx: tokio::sync::mpsc::UnboundedSender<crate::event::Message>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(100));
        let mut idx = 0usize;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let line = LINES[idx % LINES.len()].to_owned();
                    idx += 1;
                    if tx.send(crate::event::Message::Serial(line)).is_err() {
                        break;
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    });
}

/// Spawns a task that sends synthetic device info after a short delay, for
/// UI development without hardware.
///
/// # Arguments
///
/// * `tx` - Sender for forwarding the info as a
///   [`crate::event::Message::DeviceInfo`] event.
pub(crate) fn spawn_device_info(
    tx: tokio::sync::mpsc::UnboundedSender<crate::event::Message>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let info = crate::flash::DeviceInfo::new(
            "ESP32-S3 (rev v0.2)",
            "4MB",
            "AA:BB:CC:DD:EE:FF",
            Vec::new(),
        );
        let _ = tx.send(crate::event::Message::DeviceInfo(Ok(info)));
    });
}

#[cfg(test)]
mod tests {
    use super::LINES;
    use crate::log;

    #[test]
    fn all_demo_lines_parse_as_structured_entries() {
        for &line in LINES {
            let entry = log::parse_line(line);
            assert!(
                !entry.tag().is_empty(),
                "demo line did not parse as structured: {line}"
            );
        }
    }

    #[test]
    fn demo_lines_cycle_without_panic() {
        assert!(!LINES.is_empty());
        for i in 0..=(LINES.len() * 2) {
            let _ = LINES[i % LINES.len()];
        }
    }
}

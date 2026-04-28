use crossterm::event::KeyEvent;

/// Inter-task messages driven through the main `select!` event loop.
pub(crate) enum Message {
    /// A keyboard event forwarded from the blocking input reader.
    Key(KeyEvent),
    /// One decoded UTF-8 line from the serial stream (lossy).
    Serial(String),
    /// 250ms heartbeat for status-message expiry.
    Tick,
    /// The serial port was lost (I/O error or physical unplug).
    Disconnected,
    /// The serial port failed to open; carries a human-readable error.
    ConnectError(String),
    /// Background port scan; carries the current and previous detected port sets.
    PortsDetected {
        current: Vec<String>,
        previous: Vec<String>,
    },
}

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
    /// The serial port opened successfully; carries the port name, command
    /// sender, and source shutdown handle so the event loop can commit the
    /// new connection atomically.
    ConnectSuccess {
        port: String,
        cmd_tx: std::sync::mpsc::Sender<crate::serial::PortCommand>,
        src_tx: tokio::sync::watch::Sender<bool>,
    },
    /// The serial port failed to open; carries a human-readable error.
    ConnectError(String),
    /// Background port scan; carries the current and previous detected port sets.
    PortsDetected {
        current: Vec<String>,
        previous: Vec<String>,
    },
    /// Flash write progress; `current` and `total` are byte counts for the
    /// current flash segment.
    FlashProgress { current: usize, total: usize },
    /// Flash operation completed; carries the result.
    FlashDone(anyhow::Result<()>),
    /// Device info probe completed; carries the result.
    DeviceInfo(anyhow::Result<crate::flash::DeviceInfo>),
    /// Erase operation completed; carries the result.
    EraseDone(anyhow::Result<()>),
}

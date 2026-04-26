use crossterm::event::KeyEvent;

/// A message passed through the main select! loop.
pub enum Message {
    Key(KeyEvent),
    /// One decoded UTF-8 line from the serial stream (lossy).
    Serial(String),
    /// 250ms heartbeat for status-message expiry.
    Tick,
    /// The serial port was lost (I/O error or physical unplug).
    Disconnected,
    /// Background port scan; carries the current set of detected ports.
    PortsDetected(Vec<String>),
}

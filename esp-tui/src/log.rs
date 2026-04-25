use std::sync::LazyLock;

use ratatui::style::Color;
use regex::Regex;

static RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([EWIDV]) \((\d+)\) ([^:]+): (.+)$").unwrap());

/// Severity level of an ESP-IDF log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Verbose,
}

impl Level {
    /// Returns the terminal color associated with this level.
    #[must_use]
    pub fn color(&self) -> Color {
        match self {
            Self::Error => Color::Red,
            Self::Warn => Color::Yellow,
            Self::Info => Color::Green,
            Self::Debug => Color::Cyan,
            Self::Verbose => Color::White,
        }
    }

    /// Returns the display label for this level.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Verbose => "VERBOSE",
        }
    }
}

impl TryFrom<char> for Level {
    type Error = anyhow::Error;

    /// # Errors
    ///
    /// Returns an error if `c` is not a recognised ESP-IDF log level character
    /// (`E`, `W`, `I`, `D`, or `V`).
    fn try_from(c: char) -> anyhow::Result<Self> {
        match c {
            'E' => Ok(Self::Error),
            'W' => Ok(Self::Warn),
            'I' => Ok(Self::Info),
            'D' => Ok(Self::Debug),
            'V' => Ok(Self::Verbose),
            _ => Err(anyhow::anyhow!("unknown log level char: {c}")),
        }
    }
}

/// A single parsed or raw line from the serial stream.
#[derive(Debug, Clone)]
pub struct Entry {
    level: Level,
    tag: String,
    message: String,
    raw: String,
}

impl Entry {
    fn parsed(level: Level, tag: &str, message: &str, raw: &str) -> Self {
        Self {
            level,
            tag: tag.trim().to_owned(),
            message: message.to_owned(),
            raw: raw.to_owned(),
        }
    }

    fn from_raw_line(line: &str) -> Self {
        Self {
            level: Level::Verbose,
            tag: String::new(),
            message: line.to_owned(),
            raw: line.to_owned(),
        }
    }

    /// Returns the severity level of this entry.
    #[must_use]
    pub fn level(&self) -> &Level {
        &self.level
    }

    /// Returns the ESP-IDF tag, or an empty string for raw (unparsed) lines.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Returns the log message body.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the original unmodified line as received from the serial stream.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

/// Parses a single line from the serial stream into a log [`Entry`].
///
/// Lines matching the ESP-IDF format `L (timestamp) TAG: message` are fully
/// parsed. All other lines are returned as [`Level::Verbose`] raw entries with
/// the original text as the message.
///
/// # Arguments
///
/// * `line` - A single newline-free line of serial output.
///
/// # Returns
///
/// A parsed [`Entry`]. This function is infallible; unrecognised lines become
/// raw entries.
#[must_use]
pub fn parse_line(line: &str) -> Entry {
    RE.captures(line).map_or_else(
        || Entry::from_raw_line(line),
        |caps| {
            let level_char = caps[1].chars().next().unwrap_or('V');
            Level::try_from(level_char).map_or_else(
                |_| Entry::from_raw_line(line),
                |level| Entry::parsed(level, &caps[3], &caps[4], line),
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_info_line() {
        let e = parse_line("I (1234) wifi: Connected to AP");
        assert_eq!(e.level(), &Level::Info);
        assert_eq!(e.tag(), "wifi");
        assert_eq!(e.message(), "Connected to AP");
    }

    #[test]
    fn parses_error_line() {
        let e = parse_line("E (9999) i2c: Timeout on addr 0x3C");
        assert_eq!(e.level(), &Level::Error);
        assert_eq!(e.tag(), "i2c");
        assert_eq!(e.message(), "Timeout on addr 0x3C");
    }

    #[test]
    fn raw_line_on_no_match() {
        let e = parse_line("some raw output");
        assert_eq!(e.level(), &Level::Verbose);
        assert_eq!(e.tag(), "");
        assert_eq!(e.message(), "some raw output");
        assert_eq!(e.raw(), "some raw output");
    }

    #[test]
    fn level_try_from_all_chars() {
        assert_eq!(Level::try_from('E').unwrap(), Level::Error);
        assert_eq!(Level::try_from('W').unwrap(), Level::Warn);
        assert_eq!(Level::try_from('I').unwrap(), Level::Info);
        assert_eq!(Level::try_from('D').unwrap(), Level::Debug);
        assert_eq!(Level::try_from('V').unwrap(), Level::Verbose);
        assert!(Level::try_from('X').is_err());
    }
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;
use serde::Deserialize;

// ---- Color defaults ----

fn default_error_color() -> Color {
    Color::Red
}
fn default_warn_color() -> Color {
    Color::Yellow
}
fn default_info_color() -> Color {
    Color::Green
}
fn default_debug_color() -> Color {
    Color::Cyan
}
fn default_verbose_color() -> Color {
    Color::White
}
fn default_focused_border_color() -> Color {
    Color::Cyan
}
fn default_port_connected_color() -> Color {
    Color::Green
}
fn default_port_disconnected_color() -> Color {
    Color::Red
}
fn default_scroll_indicator_color() -> Color {
    Color::Yellow
}
fn default_hint_text_color() -> Color {
    Color::DarkGray
}
fn default_filter_bar_color() -> Color {
    Color::Yellow
}
fn default_log_tag_color() -> Color {
    Color::DarkGray
}
fn default_log_unparsed_color() -> Color {
    Color::DarkGray
}
fn default_search_label_color() -> Color {
    Color::DarkGray
}
fn default_search_query_color() -> Color {
    Color::Yellow
}
fn default_status_message_color() -> Color {
    Color::Yellow
}
fn default_cpu_low_color() -> Color {
    Color::Green
}
fn default_cpu_medium_color() -> Color {
    Color::Yellow
}
fn default_cpu_high_color() -> Color {
    Color::Red
}
fn default_metric_bar_color() -> Color {
    Color::Green
}
fn default_flash_message_color() -> Color {
    Color::Yellow
}
fn default_flash_gauge_color() -> Color {
    Color::Green
}
fn default_confirm_title_color() -> Color {
    Color::Red
}
fn default_confirm_ok_color() -> Color {
    Color::Green
}
fn default_confirm_cancel_color() -> Color {
    Color::DarkGray
}

// ---- Numeric defaults ----

fn default_buffer_size() -> usize {
    10_000
}
fn default_sparkline_len() -> usize {
    60
}
fn default_monitor_pct() -> u16 {
    60
}

// ---- Color parsing / serialization ----

fn deserialize_color<'de, D>(d: D) -> Result<Color, D::Error>
where
    D: serde::Deserializer<'de>,
{
    parse_color(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

/// Parses a color string into a ratatui [`Color`].
///
/// Accepts lowercase named colors (`"red"`, `"dark_gray"`, etc.) and
/// `"#RRGGBB"` hex strings.
///
/// # Arguments
///
/// * `s` - The color string to parse.
///
/// # Returns
///
/// The corresponding [`Color`] value.
///
/// # Errors
///
/// Returns an error if the string is not a recognized color name or valid hex.
pub(crate) fn parse_color(s: &str) -> anyhow::Result<Color> {
    if let Some(hex) = s.strip_prefix('#') {
        anyhow::ensure!(hex.len() == 6, "hex color must be 6 digits: #{hex}");
        let rgb = u32::from_str_radix(hex, 16)
            .with_context(|| format!("invalid hex color: #{hex}"))?;
        Ok(Color::Rgb(
            ((rgb >> 16) & 0xFF) as u8,
            ((rgb >> 8) & 0xFF) as u8,
            (rgb & 0xFF) as u8,
        ))
    } else {
        match s {
            "black" => Ok(Color::Black),
            "red" => Ok(Color::Red),
            "green" => Ok(Color::Green),
            "yellow" => Ok(Color::Yellow),
            "blue" => Ok(Color::Blue),
            "magenta" => Ok(Color::Magenta),
            "cyan" => Ok(Color::Cyan),
            "gray" => Ok(Color::Gray),
            "dark_gray" => Ok(Color::DarkGray),
            "light_red" => Ok(Color::LightRed),
            "light_green" => Ok(Color::LightGreen),
            "light_yellow" => Ok(Color::LightYellow),
            "light_blue" => Ok(Color::LightBlue),
            "light_magenta" => Ok(Color::LightMagenta),
            "light_cyan" => Ok(Color::LightCyan),
            "white" => Ok(Color::White),
            "reset" => Ok(Color::Reset),
            _ => Err(anyhow::anyhow!("unknown color: {s}")),
        }
    }
}

// ---- Key parsing ----

/// Parses a key string into a crossterm `(KeyCode, KeyModifiers)` pair.
///
/// Accepts formats like `"j"`, `"ctrl+f"`, `"alt+v"`, `"F5"`, `"enter"`,
/// `"esc"`, `"pageup"`, `"pagedown"`, `"up"`, `"down"`, `"left"`, `"right"`,
/// `"tab"`, `"backspace"`, `"delete"`, `"home"`, `"end"`.
///
/// # Arguments
///
/// * `s` - The key string to parse.
///
/// # Returns
///
/// A `(KeyCode, KeyModifiers)` pair.
///
/// # Errors
///
/// Returns an error if the string is not a recognized key descriptor.
pub(crate) fn parse_key(s: &str) -> anyhow::Result<(KeyCode, KeyModifiers)> {
    let parts: Vec<&str> = s.split('+').collect();
    let (key_part, mod_parts) = parts
        .split_last()
        .ok_or_else(|| anyhow::anyhow!("empty key string"))?;

    let modifiers = mod_parts.iter().try_fold(KeyModifiers::empty(), |acc, m| {
        match m.to_lowercase().as_str() {
            "ctrl" => Ok(acc | KeyModifiers::CONTROL),
            "alt" => Ok(acc | KeyModifiers::ALT),
            "shift" => Ok(acc | KeyModifiers::SHIFT),
            other => Err(anyhow::anyhow!("unknown modifier: {other}")),
        }
    })?;

    let code = parse_key_code(key_part)?;
    let modifiers = match code {
        KeyCode::Char(c) if char_needs_shift(c) => modifiers | KeyModifiers::SHIFT,
        _ => modifiers,
    };
    Ok((code, modifiers))
}

fn char_needs_shift(c: char) -> bool {
    c.is_ascii_uppercase()
        || matches!(
            c,
            '~' | '!'
                | '@'
                | '#'
                | '$'
                | '%'
                | '^'
                | '&'
                | '*'
                | '('
                | ')'
                | '_'
                | '+'
                | '{'
                | '}'
                | '|'
                | ':'
                | '"'
                | '<'
                | '>'
                | '?'
        )
}

fn parse_key_code(s: &str) -> anyhow::Result<KeyCode> {
    let f_num = s
        .strip_prefix('F')
        .or_else(|| s.strip_prefix('f'))
        .and_then(|n| n.parse::<u8>().ok());
    if s.len() == 1 {
        Ok(KeyCode::Char(s.chars().next().expect("checked len == 1")))
    } else if let Some(num) = f_num {
        Ok(KeyCode::F(num))
    } else {
        match s.to_lowercase().as_str() {
            "enter" => Ok(KeyCode::Enter),
            "esc" => Ok(KeyCode::Esc),
            "tab" => Ok(KeyCode::Tab),
            "backtab" => Ok(KeyCode::BackTab),
            "backspace" => Ok(KeyCode::Backspace),
            "delete" => Ok(KeyCode::Delete),
            "insert" => Ok(KeyCode::Insert),
            "home" => Ok(KeyCode::Home),
            "end" => Ok(KeyCode::End),
            "pageup" => Ok(KeyCode::PageUp),
            "pagedown" => Ok(KeyCode::PageDown),
            "up" => Ok(KeyCode::Up),
            "down" => Ok(KeyCode::Down),
            "left" => Ok(KeyCode::Left),
            "right" => Ok(KeyCode::Right),
            other => Err(anyhow::anyhow!("unknown key: {other}")),
        }
    }
}

// ---- Preset loading ----

/// Loads a key-binding preset by name or file path.
///
/// Built-in presets (`"vim"`, `"emacs"`) are embedded in the binary. Any
/// other value is treated as a path to a TOML file on disk.
///
/// # Arguments
///
/// * `name_or_path` - A built-in preset name or a path to a `.toml` file.
///
/// # Returns
///
/// A map of key strings to action strings.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed.
pub(crate) fn load_preset_overrides(
    name_or_path: &str,
) -> anyhow::Result<HashMap<String, String>> {
    let content: std::borrow::Cow<str> = match name_or_path {
        "vim" => std::borrow::Cow::Borrowed(include_str!("../presets/vim.toml")),
        "emacs" => std::borrow::Cow::Borrowed(include_str!("../presets/emacs.toml")),
        path => std::borrow::Cow::Owned(
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read preset file: {path}"))?,
        ),
    };
    toml::from_str(&content).context("failed to parse preset TOML")
}

// ---- Config sections ----

/// Colors for ESP-IDF log severity levels.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LogColors {
    #[serde(
        default = "default_error_color",
        deserialize_with = "deserialize_color"
    )]
    pub error: Color,
    #[serde(default = "default_warn_color", deserialize_with = "deserialize_color")]
    pub warn: Color,
    #[serde(default = "default_info_color", deserialize_with = "deserialize_color")]
    pub info: Color,
    #[serde(
        default = "default_debug_color",
        deserialize_with = "deserialize_color"
    )]
    pub debug: Color,
    #[serde(
        default = "default_verbose_color",
        deserialize_with = "deserialize_color"
    )]
    pub verbose: Color,
}

impl Default for LogColors {
    fn default() -> Self {
        Self {
            error: default_error_color(),
            warn: default_warn_color(),
            info: default_info_color(),
            debug: default_debug_color(),
            verbose: default_verbose_color(),
        }
    }
}

/// Colors for UI chrome elements (borders, status indicators, text hints).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ChromeColors {
    #[serde(
        default = "default_focused_border_color",
        deserialize_with = "deserialize_color"
    )]
    pub focused_border: Color,
    #[serde(
        default = "default_port_connected_color",
        deserialize_with = "deserialize_color"
    )]
    pub port_connected: Color,
    #[serde(
        default = "default_port_disconnected_color",
        deserialize_with = "deserialize_color"
    )]
    pub port_disconnected: Color,
    #[serde(
        default = "default_scroll_indicator_color",
        deserialize_with = "deserialize_color"
    )]
    pub scroll_indicator: Color,
    #[serde(
        default = "default_hint_text_color",
        deserialize_with = "deserialize_color"
    )]
    pub hint_text: Color,
    #[serde(
        default = "default_filter_bar_color",
        deserialize_with = "deserialize_color"
    )]
    pub filter_bar: Color,
    #[serde(
        default = "default_log_tag_color",
        deserialize_with = "deserialize_color"
    )]
    pub log_tag: Color,
    #[serde(
        default = "default_log_unparsed_color",
        deserialize_with = "deserialize_color"
    )]
    pub log_unparsed: Color,
    #[serde(
        default = "default_search_label_color",
        deserialize_with = "deserialize_color"
    )]
    pub search_label: Color,
    #[serde(
        default = "default_search_query_color",
        deserialize_with = "deserialize_color"
    )]
    pub search_query: Color,
    #[serde(
        default = "default_status_message_color",
        deserialize_with = "deserialize_color"
    )]
    pub status_message: Color,
}

impl Default for ChromeColors {
    fn default() -> Self {
        Self {
            focused_border: default_focused_border_color(),
            port_connected: default_port_connected_color(),
            port_disconnected: default_port_disconnected_color(),
            scroll_indicator: default_scroll_indicator_color(),
            hint_text: default_hint_text_color(),
            filter_bar: default_filter_bar_color(),
            log_tag: default_log_tag_color(),
            log_unparsed: default_log_unparsed_color(),
            search_label: default_search_label_color(),
            search_query: default_search_query_color(),
            status_message: default_status_message_color(),
        }
    }
}

/// Colors for the System Inspector pane metrics.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct InspectorColors {
    #[serde(
        default = "default_cpu_low_color",
        deserialize_with = "deserialize_color"
    )]
    pub cpu_low: Color,
    #[serde(
        default = "default_cpu_medium_color",
        deserialize_with = "deserialize_color"
    )]
    pub cpu_medium: Color,
    #[serde(
        default = "default_cpu_high_color",
        deserialize_with = "deserialize_color"
    )]
    pub cpu_high: Color,
    #[serde(
        default = "default_metric_bar_color",
        deserialize_with = "deserialize_color"
    )]
    pub metric_bar: Color,
}

impl Default for InspectorColors {
    fn default() -> Self {
        Self {
            cpu_low: default_cpu_low_color(),
            cpu_medium: default_cpu_medium_color(),
            cpu_high: default_cpu_high_color(),
            metric_bar: default_metric_bar_color(),
        }
    }
}

/// Colors for flash/erase operation UI elements.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FlashColors {
    #[serde(
        default = "default_flash_message_color",
        deserialize_with = "deserialize_color"
    )]
    pub message: Color,
    #[serde(
        default = "default_flash_gauge_color",
        deserialize_with = "deserialize_color"
    )]
    pub gauge: Color,
    #[serde(
        default = "default_confirm_title_color",
        deserialize_with = "deserialize_color"
    )]
    pub confirm_title: Color,
    #[serde(
        default = "default_confirm_ok_color",
        deserialize_with = "deserialize_color"
    )]
    pub confirm_ok: Color,
    #[serde(
        default = "default_confirm_cancel_color",
        deserialize_with = "deserialize_color"
    )]
    pub confirm_cancel: Color,
}

impl Default for FlashColors {
    fn default() -> Self {
        Self {
            message: default_flash_message_color(),
            gauge: default_flash_gauge_color(),
            confirm_title: default_confirm_title_color(),
            confirm_ok: default_confirm_ok_color(),
            confirm_cancel: default_confirm_cancel_color(),
        }
    }
}

/// All color configuration, grouped by UI section.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub(crate) struct ColorsConfig {
    pub log: LogColors,
    pub chrome: ChromeColors,
    pub inspector: InspectorColors,
    pub flash: FlashColors,
}

/// Serial port configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub(crate) struct SerialConfig {
    /// Port name to auto-connect to.
    pub port: Option<String>,
    /// Baud rate.
    pub baud: Option<u32>,
}

/// Flash / ELF configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub(crate) struct FlashConfig {
    /// ELF path to pre-populate in the flash selector on startup.
    pub elf_path: Option<PathBuf>,
}

/// UI layout and buffer configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct UiConfig {
    /// Which pane to show on startup (`"monitor"` or `"inspector"`);
    /// absent means split view.
    pub initial_pane: Option<String>,
    /// Initial width of the Serial Monitor pane as a percentage of the split
    /// area, in the range `[0, 100]`. Only applies in split view.
    #[serde(default = "default_monitor_pct")]
    pub monitor_pct: u16,
    /// Log ring-buffer capacity in lines.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
    /// Number of sparkline samples retained per channel.
    #[serde(default = "default_sparkline_len")]
    pub sparkline_len: usize,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            initial_pane: None,
            monitor_pct: default_monitor_pct(),
            buffer_size: default_buffer_size(),
            sparkline_len: default_sparkline_len(),
        }
    }
}

/// Keybinding configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub(crate) struct KeysConfig {
    /// Built-in preset name (`"vim"`, `"emacs"`) or path to a `.toml` file.
    pub preset: Option<String>,
    /// Per-key overrides, merged on top of the preset.
    /// Keys are strings like `"j"` or `"ctrl+n"`; values are action names
    /// like `"scroll_down"`.
    #[serde(default)]
    pub overrides: HashMap<String, String>,
}

/// Top-level configuration loaded from `esp-tui.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub(crate) struct Config {
    pub serial: SerialConfig,
    pub flash: FlashConfig,
    pub ui: UiConfig,
    pub colors: ColorsConfig,
    pub keys: KeysConfig,
}

// ---- Deep TOML merge ----

fn merge_toml(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                merge_toml(
                    b.entry(k)
                        .or_insert(toml::Value::Table(toml::map::Map::default())),
                    v,
                );
            }
        }
        (base, over) => *base = over,
    }
}

// ---- Config loading ----

fn read_toml_file(path: &Path) -> anyhow::Result<Option<toml::Value>> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s)
            .map(Some)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(e).with_context(|| format!("failed to read {}", path.display()))
        }
    }
}

/// Loads configuration from up to two TOML files, merged together.
///
/// When `explicit_path` is `Some`, only that file is loaded (no global or
/// project-local search). Otherwise:
///
/// 1. Global: `{config_dir}/esp-tui/config.toml`
/// 2. Project-local: `./esp-tui.toml`
///
/// Project-local keys override global keys at every nesting level. Missing
/// keys fall back to their per-field defaults in the config structs.
///
/// # Arguments
///
/// * `explicit_path` - Optional path to a specific config file.
///
/// # Returns
///
/// A fully resolved [`Config`].
///
/// # Errors
///
/// Returns an error if a file exists but cannot be read or contains invalid
/// TOML.
pub(crate) fn load(explicit_path: Option<&Path>) -> anyhow::Result<Config> {
    let merged = if let Some(path) = explicit_path {
        read_toml_file(path)?
    } else {
        let global_path =
            dirs::config_dir().map(|d| d.join("esp-tui").join("config.toml"));
        let local_path = Path::new("esp-tui.toml");

        let global = global_path.as_deref().and_then(|p| {
            read_toml_file(p)
                .map_err(|e| eprintln!("warning: could not read global config: {e}"))
                .ok()
                .flatten()
        });
        let local = read_toml_file(local_path)?;

        match (global, local) {
            (Some(mut g), Some(l)) => {
                merge_toml(&mut g, l);
                Some(g)
            }
            (Some(g), None) => Some(g),
            (None, Some(l)) => Some(l),
            (None, None) => None,
        }
    };

    merged.map_or(Ok(Config::default()), |v| {
        v.try_into().context("failed to deserialize config")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_color_named() {
        assert_eq!(parse_color("red").unwrap(), Color::Red);
        assert_eq!(parse_color("cyan").unwrap(), Color::Cyan);
        assert_eq!(parse_color("dark_gray").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("white").unwrap(), Color::White);
    }

    #[test]
    fn parse_color_hex() {
        assert_eq!(
            parse_color("#ff0000").unwrap(),
            Color::Rgb(0xff, 0x00, 0x00)
        );
        assert_eq!(
            parse_color("#1a2b3c").unwrap(),
            Color::Rgb(0x1a, 0x2b, 0x3c)
        );
    }

    #[test]
    fn parse_color_unknown_returns_error() {
        assert!(parse_color("chartreuse").is_err());
        assert!(parse_color("#gg0000").is_err());
        assert!(parse_color("#fff").is_err());
    }

    #[test]
    fn parse_key_single_char() {
        let (code, mods) = parse_key("j").unwrap();
        assert_eq!(code, KeyCode::Char('j'));
        assert_eq!(mods, KeyModifiers::empty());
    }

    #[test]
    fn parse_key_ctrl_modifier() {
        let (code, mods) = parse_key("ctrl+n").unwrap();
        assert_eq!(code, KeyCode::Char('n'));
        assert_eq!(mods, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_key_alt_modifier() {
        let (code, mods) = parse_key("alt+v").unwrap();
        assert_eq!(code, KeyCode::Char('v'));
        assert_eq!(mods, KeyModifiers::ALT);
    }

    #[test]
    fn parse_key_named_keys() {
        assert_eq!(parse_key("enter").unwrap().0, KeyCode::Enter);
        assert_eq!(parse_key("esc").unwrap().0, KeyCode::Esc);
        assert_eq!(parse_key("up").unwrap().0, KeyCode::Up);
        assert_eq!(parse_key("pageup").unwrap().0, KeyCode::PageUp);
        assert_eq!(parse_key("F5").unwrap().0, KeyCode::F(5));
    }

    #[test]
    fn parse_key_uppercase_char_includes_shift() {
        let (code, mods) = parse_key("G").unwrap();
        assert_eq!(code, KeyCode::Char('G'));
        assert_eq!(mods, KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_key_shifted_symbol_includes_shift() {
        let (code, mods) = parse_key(">").unwrap();
        assert_eq!(code, KeyCode::Char('>'));
        assert_eq!(mods, KeyModifiers::SHIFT);

        let (code, mods) = parse_key("<").unwrap();
        assert_eq!(code, KeyCode::Char('<'));
        assert_eq!(mods, KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_key_alt_with_shifted_symbol_includes_shift() {
        let (code, mods) = parse_key("alt+>").unwrap();
        assert_eq!(code, KeyCode::Char('>'));
        assert_eq!(mods, KeyModifiers::ALT | KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_key_unknown_returns_error() {
        assert!(parse_key("bogus_key").is_err());
        assert!(parse_key("super+x").is_err());
    }

    #[test]
    fn default_config_matches_hardcoded_values() {
        let cfg = Config::default();
        assert_eq!(cfg.ui.buffer_size, 10_000);
        assert_eq!(cfg.ui.sparkline_len, 60);
        assert_eq!(cfg.colors.log.error, Color::Red);
        assert_eq!(cfg.colors.log.warn, Color::Yellow);
        assert_eq!(cfg.colors.log.info, Color::Green);
        assert_eq!(cfg.colors.log.debug, Color::Cyan);
        assert_eq!(cfg.colors.log.verbose, Color::White);
        assert_eq!(cfg.colors.chrome.focused_border, Color::Cyan);
    }

    #[test]
    fn load_no_files_returns_defaults() {
        let cfg = load(Some(Path::new("/nonexistent_esp_tui_config.toml"))).unwrap();
        assert_eq!(cfg.ui.buffer_size, 10_000);
        assert_eq!(cfg.colors.log.error, Color::Red);
    }

    #[test]
    fn load_partial_override() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("esp-tui.toml");
        std::fs::write(
            &path,
            r#"
[colors.log]
error = "magenta"
"#,
        )
        .unwrap();
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.colors.log.error, Color::Magenta);
        assert_eq!(cfg.colors.log.warn, Color::Yellow);
        assert_eq!(cfg.ui.buffer_size, 10_000);
    }

    #[test]
    fn load_keys_preset_and_overrides() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("esp-tui.toml");
        std::fs::write(
            &path,
            r#"
[keys]
preset = "vim"

[keys.overrides]
"ctrl+q" = "quit"
"#,
        )
        .unwrap();
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.keys.preset, Some("vim".to_owned()));
        assert_eq!(
            cfg.keys.overrides.get("ctrl+q").map(String::as_str),
            Some("quit")
        );
    }

    #[test]
    fn load_serial_and_flash_config() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("esp-tui.toml");
        std::fs::write(
            &path,
            r#"
[serial]
baud = 9600

[flash]
elf_path = "/tmp/firmware.elf"
"#,
        )
        .unwrap();
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.serial.baud, Some(9600));
        assert_eq!(cfg.flash.elf_path, Some(PathBuf::from("/tmp/firmware.elf")));
    }
}

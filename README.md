# esp-tui

[![CI](https://github.com/yrakcaz/esp-tui/actions/workflows/ci.yml/badge.svg)](https://github.com/yrakcaz/esp-tui/actions/workflows/ci.yml)
[![MIT License](https://img.shields.io/github/license/yrakcaz/esp-tui?color=blue)](./LICENSE)

ESP32 developer workstation for the terminal. A persistent ratatui TUI combining
serial monitoring, flash controls, and live device telemetry into a single interface.
Works with any ESP32 firmware: C, C++, Rust, Arduino.

---

## Features

**Phase 1**

- ESP-IDF log parsing with color-coded severity levels: `ERROR` `WARN` `INFO` `DEBUG` `VERBOSE`
- Tag-based filtering: show or hide output by ESP-IDF component tag
- Scrollable log history with a configurable 10 000-line ring buffer
- Port auto-detection: connects automatically when one ESP32 is found, opens a
  selector popup when multiple are present
- Hardware reset via DTR/RTS (`r`)
- `--demo` flag: synthetic log output for UI development without hardware
- `Ctrl-L` to clear the log on demand

**Phase 2 (current)**

- Board info probe on connect: chip type, revision, flash size, and MAC address displayed in the inspector pane
- ELF firmware flashing via espflash with a live progress gauge (`f`)
- Full-flash erase with confirmation prompt (`e`)
- ELF path selector popup with filesystem tab-completion, opened by `f`
- `--baud <rate>` CLI flag
- Port auto-reconnect after flash or erase

---

## Installation

```
cargo install --git https://github.com/yrakcaz/esp-tui esp-tui
```

Or build from source:

```
git clone https://github.com/yrakcaz/esp-tui
cd esp-tui
cargo install --path esp-tui
```

---

## Usage

```
esp-tui [OPTIONS]

Options:
  -p, --port <PORT>  Serial port to connect to
  -b, --baud <BAUD>  Serial baud rate (default: 115200)
      --demo         Run in demo mode with synthetic log output (no hardware required)
  -h, --help         Print help
```

**Examples**

```
esp-tui                          # auto-detect port
esp-tui --port /dev/ttyUSB0      # connect to a specific port
esp-tui --demo                   # run without hardware
```

---

## Keybindings

| Key | Action |
|---|---|
| `c` | Connect / scan for ports |
| `d` | Disconnect |
| `f` | Open ELF path selector and flash to device |
| `e` | Erase flash (shows confirmation prompt) |
| `r` | Reset device (DTR/RTS) |
| `Tab` | Open / close filter popup |
| `Space` | Toggle filter item (inside popup) |
| `Ctrl-A` | Toggle all filter items (inside popup) |
| `↑` / `↓` | Scroll up / down |
| `PgUp` / `PgDn` | Scroll by 10 lines |
| `Ctrl-L` | Clear log buffer |
| `q` / `Esc` | Exit scroll mode, or quit |
| `Ctrl-C` | Quit |

---

## Roadmap

| Phase | Description | Status |
|---|---|---|
| 1 | Serial monitor MVP | Complete |
| 2 | Flash integration (espflash, progress bar, board info) | Complete |
| 3 | Agent + System Inspector (heap, CPU, task list) | Planned |
| 4 | Polish (backtrace decoding, sparklines, defmt, multi-device) | Planned |

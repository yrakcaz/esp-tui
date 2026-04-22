# TODO

## Phase 1: Serial Monitor MVP

- [ ] Basic ratatui layout: monitor pane (top-left), inspector pane (top-right), flash bar (bottom)
- [ ] Status bar with keybinding hints
- [ ] Async serial port reader (tokio task)
- [ ] Raw UART stream rendered in monitor pane
- [ ] ESP-IDF log format parsing: level + tag + message
- [ ] Color coding by log level (ERROR=red, WARN=yellow, INFO=green, DEBUG=cyan, VERBOSE=white)
- [ ] Tag-based filtering (show/hide by ESP-IDF tag)
- [ ] Scrollable log history with configurable buffer size
- [ ] `Ctrl-R`: send reset via DTR/RTS
- [ ] `Ctrl-F`: invoke flash (espflash stub)
- [ ] Port auto-detection: scan for ESP32 devices, selector if multiple found
- [ ] `--demo` flag: emit synthetic ESP-IDF log lines without hardware
- [ ] `q` / `Ctrl-C` exits cleanly, restores terminal

## Phase 2: Flash Integration

- [ ] espflash library integration (not subprocess)
- [ ] Flash progress bar rendered in bottom pane during flash
- [ ] Board info display on connect (chip type, revision, flash size, MAC)
- [ ] Partition table viewer popup
- [ ] `--elf <path>` CLI flag
- [ ] `--port` and `--baud` CLI flags
- [ ] Port auto-reconnect after reset/flash cycle
- [ ] Erase flash with confirmation prompt

## Phase 3: Agent + System Inspector

- [ ] `esp32-tui-agent` crate: FreeRTOS task, heap/CPU/task sampling
- [ ] COBS framing with magic header `0xAE 0x73`
- [ ] `postcard` serialization of `TelemetryFrame`
- [ ] C ABI: `esp32_tui_agent_start()` via `#[no_mangle] extern "C"`
- [ ] Pre-compiled `.a` variants for all chip targets, bundled via `include_bytes!`
- [ ] Host-side COBS demuxer (splits agent frames from plain log lines)
- [ ] System Inspector pane: heap gauges, per-core CPU bars, task list
- [ ] Agent detection / graceful absence ("agent not detected" prompt)
- [ ] In-TUI agent install flow (`[A]` keybinding)
- [ ] `esp32-tui agent install` CLI subcommand

## Release

- [ ] Make the repo public
- [ ] Add CI and license badges to README
- [ ] Publish `esp32-tui` to crates.io
- [ ] Add crates.io version badge to README
- [ ] Pre-built binaries via GitHub Releases (Linux x86_64, macOS x86_64/ARM64)
- [ ] Homebrew tap
- [ ] Install script (`curl -fsSL https://esp32-tui.dev/install.sh | sh`)

## Phase 4: Polish

- [ ] Panic/backtrace decoder: addr2line + ELF symbol resolution
- [ ] Historical sparklines for heap and CPU over last N seconds
- [ ] WiFi stats in inspector (RSSI, channel, TX/RX counts)
- [ ] Defmt binary log format support
- [ ] Multi-device tab switching
- [ ] `esp32-tui.toml` config file (port, baud, ELF path, agent options)
- [ ] `--project` flag to auto-detect ELF from Cargo project
- [ ] Mouse support for scrolling log pane

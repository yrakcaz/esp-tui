# TODO

## Phase 1: Serial Monitor MVP

- [x] Basic ratatui layout: monitor pane (top-left), inspector pane (top-right), flash bar (bottom)
- [x] Status bar with keybinding hints
- [x] Async serial port reader (tokio task)
- [x] Raw UART stream rendered in monitor pane
- [x] ESP-IDF log format parsing: level + tag + message
- [x] Color coding by log level (ERROR=red, WARN=yellow, INFO=green, DEBUG=cyan, VERBOSE=white)
- [x] Tag-based filtering (show/hide by ESP-IDF tag)
- [x] Scrollable log history with configurable buffer size
- [x] `r`: send reset via DTR/RTS
- [x] `f` / `e` / `c`: stub keybindings (Phase 2)
- [x] Port auto-detection: scan for ESP32 devices, selector if multiple found
- [x] `--demo` flag: emit synthetic ESP-IDF log lines without hardware
- [x] `q` / `Ctrl-C` exits cleanly, restores terminal

## Phase 2: Flash Integration

- [ ] espflash library integration (not subprocess)
- [ ] Flash progress bar rendered in bottom pane during flash
- [ ] Board info display on connect (chip type, revision, flash size, MAC)
- [ ] Partition table viewer popup
- [ ] `--elf <path>` CLI flag
- [ ] `--port`, `--baud`, and `--elf` CLI flags
- [ ] Port auto-reconnect after reset/flash cycle
- [ ] Erase flash with confirmation prompt

## Phase 3: Agent + System Inspector

- [ ] `esp-tui-agent` crate: FreeRTOS task, heap/CPU/task sampling
- [ ] COBS framing with magic header `0xAE 0x73`
- [ ] `postcard` serialization of `TelemetryFrame`
- [ ] C ABI: `esp_tui_agent_start()` via `#[no_mangle] extern "C"`
- [ ] Pre-compiled `.a` variants for all chip targets, bundled via `include_bytes!`
- [ ] Host-side COBS demuxer (splits agent frames from plain log lines)
- [ ] System Inspector pane: heap gauges, per-core CPU bars, task list
- [ ] Agent detection / graceful absence ("agent not detected" prompt)
- [ ] In-TUI agent install flow (`[A]` keybinding)
- [ ] `esp-tui agent install` CLI subcommand

## Release

- [ ] Make the repo public
- [ ] Add CI and license badges to README
- [ ] Publish `esp-tui` to crates.io
- [ ] Add crates.io version badge to README
- [ ] Pre-built binaries via GitHub Releases (Linux x86_64, macOS x86_64/ARM64)
- [ ] Homebrew tap
- [ ] Install script (`curl -fsSL https://esp-tui.dev/install.sh | sh`)

## Phase 4: Polish

- [ ] Panic/backtrace decoder: addr2line + ELF symbol resolution
- [ ] Historical sparklines for heap and CPU over last N seconds
- [ ] WiFi stats in inspector (RSSI, channel, TX/RX counts)
- [ ] Defmt binary log format support
- [ ] Multi-device tab switching
- [ ] `esp-tui.toml` config file (port, baud, ELF path, buffer size, agent options)
- [ ] `--project` flag to auto-detect ELF from Cargo project
- [ ] Mouse support for scrolling log pane

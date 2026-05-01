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

- [x] espflash library integration (not subprocess)
- [x] Flash progress bar rendered in bottom pane during flash
- [x] Board info display on connect (chip type, revision, flash size, MAC)
- [ ] Partition table viewer popup (deferred: no direct espflash API without stub/temp file)
- [x] `--elf <path>` CLI flag
- [x] `--baud` CLI flag; `--port` was already present
- [x] Port auto-reconnect after reset/flash cycle
- [x] Erase flash with confirmation prompt
- [x] ELF path selector popup with tab-completion (`s` keybinding)

## Phase 3: Agent + System Inspector

- [ ] `esp-tui-agent` crate: FreeRTOS task, heap/CPU/task sampling
- [ ] COBS framing with magic header `0xAE 0x73`
- [ ] `postcard` serialization of `TelemetryFrame`
- [ ] C ABI: `esp_tui_agent_start()` via `#[no_mangle] extern "C"`
- [ ] Pre-compiled `.a` variants for all chip targets, bundled via `include_bytes!`
- [ ] Define a `Source` trait to unify `serial::Port` and the agent telemetry stream behind a common interface
- [ ] Host-side COBS demuxer (splits agent frames from plain log lines)
- [ ] System Inspector pane: heap gauges, per-core CPU bars, task list
- [ ] Agent detection / graceful absence ("agent not detected" prompt)
- [ ] In-TUI agent install flow (`[A]` keybinding)
- [ ] `esp-tui agent install` CLI subcommand
- [ ] Pane focus: introduce a `FocusedPane` enum so scroll and resize operations target the active pane; review and reassign conflicting keybindings (e.g. `Tab` currently opens the filter popup) at that time
- [ ] Per-pane independent scrolling once Inspector has scrollable content
- [ ] Per-pane independent resizing (adjust split ratio with keybindings)
- [ ] Split `app.rs`: move `run_inner`, `begin_connect`, `spawn_port_poller`, `handle_ports_detected`, and `apply_scan` into a new `runner.rs`; `app.rs` becomes a pure state container. The seam already exists but the split is not worth the churn until agent state grows the file further.

## Phase 4: Polish

- [ ] On macOS, filter `cu.*` entries from port detection: only `tty.*` devices should appear in the selector and auto-connect logic, since `cu.*` is not the correct interface for ESP32 serial communication
- [ ] Panic/backtrace decoder: addr2line + ELF symbol resolution
- [ ] Historical sparklines for heap and CPU over last N seconds
- [ ] WiFi stats in inspector (RSSI, channel, TX/RX counts)
- [ ] Defmt binary log format support
- [ ] Multi-device tab switching
- [ ] `esp-tui.toml` config file (port, baud, ELF path, buffer size, agent options)
- [ ] Configurable keybindings, potentially with preset modes (e.g. vim, emacs); keybindings like j/k were intentionally left out for now pending this
- [ ] Configurable color scheme (log level colors, UI chrome) via esp-tui.toml
- [ ] `--project` flag to auto-detect ELF from Cargo project
- [ ] Mouse support for scrolling log pane
- [ ] Log search: `/` opens an inline search bar at the bottom of the monitor pane (like tmux/less); `n`/`N` to jump between matches with highlighting
- [ ] Log regex filter: live filter mode that hides non-matching lines, stacking on top of the existing level/tag filter; consider reusing the `/` entry point with a toggle between highlight and filter modes

## Release

- [x] Make the repo public
- [x] Add CI and license badges to README
- [ ] Publish `esp-tui` to crates.io
- [ ] Add crates.io version badge to README
- [ ] Pre-built binaries via GitHub Releases (Linux x86_64, macOS x86_64/ARM64)
- [ ] Homebrew tap
- [ ] Install script (`curl -fsSL https://esp-tui.dev/install.sh | sh`)

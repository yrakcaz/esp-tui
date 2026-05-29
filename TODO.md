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
- [x] `q` / `Ctrl-C` exits cleanly, restores terminal

## Phase 2: Flash Integration

- [x] espflash library integration (not subprocess)
- [x] Flash progress bar rendered in bottom pane during flash
- [x] Board info display on connect (chip type, revision, flash size, MAC)
- [x] `--baud` CLI flag; `--port` was already present
- [x] Port auto-reconnect after reset/flash cycle
- [x] Erase flash with confirmation prompt
- [x] ELF path selector popup with tab-completion (`f` keybinding)

## Phase 3: Agent + System Inspector

- [x] `esp-agent` crate: FreeRTOS task, heap/CPU/WiFi/NVS/task sampling
- [x] Human-readable VERBOSE log lines (tag `esp_agent`); no binary encoding
- [x] C ABI: `esp_agent_configure(interval_ms)` for optional config override
- [x] `.init_array` constructor always included; auto-starts task with defaults
- [x] `cargo xtask build-agent`: cross-compiles `.a` for all five ESP32 targets
- [x] Host-side `esp_agent` tag detection and telemetry parsing
- [x] System Inspector pane: heap gauges, per-core CPU bars, task list
- [x] Partition table viewer in Inspector pane
- [x] Agent detection / graceful absence ("Waiting for esp-agent..." / "Connect a device to begin.")
- [x] Pane focus: `Tab` cycles Monitor/Inspector; focused pane shows cyan border; `Ctrl+F` opens filter
- [x] Per-pane independent scrolling (Inspector scroll targets task list)
- [x] Split `app.rs`: move `run_inner`, `begin_connect`, `spawn_port_poller`, `handle_ports_detected`, and `apply_scan` into a new `runner.rs`; `app.rs` becomes a pure state container.

## Phase 4: Polish

- [ ] `esp-tui agent install` CLI subcommand (deliver pre-built `.a` to user project)
- [x] Per-pane independent resizing (adjust split ratio with keybindings)
- [x] Consider surfacing currently parsed-but-unused agent fields in the inspector: reset reason (`Startup::reason`), core count (`Startup::cores`), silicon revision (`Startup::revision`), heap fragmentation (`Frame::heap_frag`), and frame timestamp (`Frame::timestamp_ms`)
- [ ] Revisit build system: once a release mechanism is in place (pre-built `.a` assets on GitHub Releases), evaluate whether `cargo xtask build-agent` is still the right developer-facing entry point, whether CI artifact caching is worth adding, and whether any of the current workarounds (`crate-type = ["lib", "staticlib"]`, `target_os` guards, `load_esp_env` parsing) can be simplified
- [ ] Revisit Rust-native integration: evaluate whether to publish `esp-agent` as a Cargo dependency with a safe `configure()` API (requires solving panic handler conflicts with `esp-idf-sys` and the linker inclusion problem without user-side `build.rs` changes)


- [ ] On macOS, filter `cu.*` entries from port detection: only `tty.*` devices should appear in the selector and auto-connect logic, since `cu.*` is not the correct interface for ESP32 serial communication
- [ ] Panic/backtrace decoder: addr2line + ELF symbol resolution
- [x] Historical sparklines for heap and CPU over last N seconds
- [x] WiFi stats in inspector (RSSI, channel, TX/RX counts)
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
- [ ] Consider extracting `esp-agent` into its own repo once it has independent users
- [ ] CI release workflow for `esp-agent`: matrix build all targets on tag push, publish `.a` files as GitHub Release assets
- [ ] Publish `esp-tui` to crates.io
- [ ] Add crates.io version badge to README
- [ ] Pre-built binaries via GitHub Releases (Linux x86_64, macOS x86_64/ARM64)
- [ ] Homebrew tap
- [ ] Install script (`curl -fsSL https://esp-tui.dev/install.sh | sh`)

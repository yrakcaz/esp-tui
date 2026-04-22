# esptui

ESP32 developer workstation for the terminal. A persistent ratatui TUI combining serial monitoring, flash controls, and live device telemetry into a single interface. Language-agnostic from the user's perspective — works with any ESP32 firmware (C, C++, Rust, Arduino).

---

## Crate Structure

| Crate | Purpose | Status |
|---|---|---|
| `esp-tui` | Host-side ratatui TUI application | Active |
| `esptui-agent` | ESP32-side no_std telemetry agent (C-ABI static lib) | Deferred to Phase 3 |

---

## Tech Stack

| Concern | Choice | Notes |
|---|---|---|
| TUI framework | `ratatui` + `crossterm` backend | |
| Async runtime | `tokio` | |
| Serial port | `serialport` | Same crate as espflash |
| Flash integration | `espflash` as a library dep | No subprocess spawning |
| Backtrace decoding | `addr2line` + `gimli` | Phase 4 |
| Framing/deserialization | `postcard` + `cobs` | Agent telemetry parsing, Phase 3 |
| CLI args | `clap` (derive feature) | |

---

## Layout

```
┌─ esptui ──────────────────────────────────────────────────────┐
│ [F]lash  [R]eset  [E]rase  [C]onnect   Port: /dev/ttyUSB0 ▼  │
├──────────────────────────┬────────────────────────────────────┤
│  Serial Monitor          │  System Inspector                  │
│                          │                                    │
│  [INFO ] WiFi connected  │  Heap:  ████████░░  142kb free    │
│  [WARN ] Stack near lim  │  CPU0:  ██████████  98%           │
│  [ERROR] Timeout on I2C  │  Tasks          Stack    State     │
│  [DEBUG] Reg read 0x3F   │  main           3.2kb    Running  │
│                          │  wifi_task      1.8kb    Blocked  │
├──────────────────────────┴────────────────────────────────────┤
│  Flash Progress  ████████████████████░░░░░░  72%             │
└───────────────────────────────────────────────────────────────┘
```

---

## Implementation Phases

### Phase 1 — Serial Monitor MVP
- Basic ratatui layout (monitor pane, inspector pane, flash bar)
- UART stream rendering
- ESP-IDF log level color coding (ERROR/WARN/INFO/DEBUG/VERBOSE)
- Tag-based filtering
- `Ctrl-R` reset, `Ctrl-F` flash (espflash)
- Port auto-detection
- `--demo` flag: synthetic log output for UI dev without hardware

### Phase 2 — Flash Integration
- espflash library integration
- Flash progress bar in bottom pane
- Board info display on connect
- Partition table viewer popup
- ELF path configuration (`--elf`, `esptui.toml`)

### Phase 3 — Agent + System Inspector
- `esptui-agent` crate (FreeRTOS task, heap/CPU/task sampling, COBS framing)
- C ABI (`esptui_agent_start()` via `#[no_mangle] extern "C"`)
- Pre-compiled `.a` variants for all chips, bundled via `include_bytes!`
- In-TUI agent install flow (`[A]` keybinding)
- `esptui agent install` CLI command
- Host-side COBS demuxer (magic header `0xAE 0x73`)
- System Inspector pane: heap gauges, CPU bars, task list

### Phase 4 — Polish
- Panic backtrace decoding (addr2line + ELF)
- Historical sparklines for heap/CPU
- WiFi stats pane
- Defmt log format support
- Multi-device tab switching
- `esptui.toml` config file

---

## Code Style

### Comments
- Do not add comments to explain what code does — the code should be clear enough to read on its own
- Only add a comment when there is a specific reason to believe the logic will not be self-evident to a reader (e.g. a non-obvious algorithm, an intentional workaround, or an external constraint)
- Never add boilerplate or redundant comments such as `// initialize`, `// return result`, or anything that merely restates the code

### Functional over Imperative
- Prefer functional style over imperative
- Avoid using `return` statements — use expression-based returns instead
- Use `match`, `if let`, `map`, `and_then`, `unwrap_or_else` over early returns
- Prefer iterator methods (`map`, `filter`, `fold`) over `for` loops with mutation

### Error Handling
- Use `anyhow::Result` for error handling
- Use `?` operator — avoid `.unwrap()` except in tests
- Prefer `ok_or_else` / `map_err` over `match` for Option/Result conversions

### Formatting & Linting
- Run `cargo fmt` after every code change (`max_width = 85` in `.rustfmt.toml`)
- Run `cargo clippy` and fix all warnings — `clippy::all`, `clippy::cargo`, and `clippy::pedantic` are denied
- Follow standard Rust naming conventions (`snake_case` for functions/modules, `PascalCase` for types)

### Dependency Management
- Only add a dependency when the code using it is actively being written
- Do not add dependencies that duplicate capabilities already provided by existing ones

## Maintenance

- **`TODO.md`** — check off items as they are completed; add new items when scope expands
- **`README.md`** — update when new features are usable or installation/usage instructions change
- **`AGENTS.md`** — update when new conventions are established or tech stack decisions are made
- **`.github/workflows/ci.yml`** — update when new system dependencies, toolchain requirements, or build steps are introduced

---

## Non-Goals

- No JTAG / debugger — use `probe-rs`
- No IDE plugin — terminal only
- No ESP-IDF SDK management — use `esp-generate`
- No Windows-first support — Linux/macOS primary, Windows best-effort

---

## Open Design Questions

1. **COBS magic header collision**: `0xAE 0x73` must never appear in valid UTF-8 log output — verify.
2. **Agent on no_std / Embassy**: initial agent targets `esp-idf-svc` (std). Embassy variant needs separate approach to FreeRTOS introspection.
3. **Defmt demuxing**: defmt frames + agent frames both on the same UART — framing strategy TBD.
4. **Flash port handoff**: espflash takes ownership of serial port during flashing — TUI must cleanly yield and reclaim without losing buffered logs.
5. **Multi-UART routing**: ESP32-S3/C3/C6 USB-JTAG-SERIAL may allow routing agent telemetry to a second virtual COM port, eliminating multiplexing entirely.

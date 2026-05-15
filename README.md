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
- `Ctrl-L` to clear the log on demand

**Phase 2**

- Board info probe on connect: chip type, revision, flash size, and MAC address displayed in the inspector pane
- ELF firmware flashing via espflash with a live progress gauge (`f`)
- Full-flash erase with confirmation prompt (`e`)
- ELF path selector popup with filesystem tab-completion, opened by `f`
- `--baud <rate>` CLI flag
- Port auto-reconnect after flash or erase

**Phase 3 (current)**

- `esp-agent`: a zero-dependency `no_std` static library you link into ESP32 firmware
- Auto-starts a FreeRTOS task on boot via an `.init_array` constructor; no changes to `app_main` required
- Emits heap, CPU, WiFi RSSI, NVS, and task-list telemetry as ESP-IDF VERBOSE log lines (tag `esp_agent`); parsed by esp-tui to populate the System Inspector pane, and readable in any serial monitor
- Optional override via `esp_agent_configure(interval_ms)` for custom sampling interval
- Builds a `.a` for all seven ESP32 targets via `cargo xtask build agent` (ESP32, S2, S3; C3/C2 and C6/H2 as both bare-metal and ESP-IDF Rust targets)
- System Inspector pane with live heap gauges, per-core CPU bars, task table, and partition viewer (in progress)

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

## Development

**TUI**

```
cargo build          # build the TUI binary (default; does not include esp-agent)
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets
cargo fmt --workspace
```

**Embedded agent**

Building `esp-agent` requires the Xtensa Rust toolchain. Install it once:

```
cargo install espup
espup install
```

Then build pre-compiled static libraries for all seven ESP32 targets:

```
cargo xtask build agent                                    # all targets
cargo xtask build agent --target xtensa-esp32s3-espidf    # one target
```

Produces `target/<triple>/release/libesp_agent.a` for each target. No environment setup is needed beyond running `espup install`; the xtask resolves the toolchain paths automatically.

**Examples**

Working reference projects live in `examples/c/` and `examples/rust/`. Each can be built with a single command from the repo root; the xtask builds the agent first and then the example:

```
cargo xtask build examples                                             # both, all targets
cargo xtask build examples rust                                        # Rust only, all targets
cargo xtask build examples c                                           # C only, all targets
cargo xtask build examples rust --target xtensa-esp32s3-espidf        # one target
```

Each command builds for all five ESP-IDF targets by default (ESP32, S2, S3, C3, C6). Pass a target triple as the second argument to build for a single chip. The xtask auto-detects the ESP-IDF installation at `~/.espressif/esp-idf/v5.3.1` for the C example; set `IDF_PATH` to override.

**Devcontainer**

Opening the repo in a devcontainer installs all prerequisites automatically, including the Xtensa toolchain.

---

## Usage

```
esp-tui [OPTIONS]

Options:
  -p, --port <PORT>  Serial port to connect to
  -b, --baud <BAUD>  Serial baud rate (default: 115200)
  -h, --help         Print help
```

**Examples**

```
esp-tui                          # auto-detect port
esp-tui --port /dev/ttyUSB0      # connect to a specific port
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

**ELF path selector** (active while the `f` popup is open)

| Key | Action |
|---|---|
| `Tab` | Tab-complete: auto-accept single match, extend to common prefix for multiple |
| `Shift-Tab` | Cycle completions backward |
| `↑` / `↓` | Move through completion list |
| `←` / `→` | Move cursor left / right |
| `Enter` | Accept highlighted completion, or confirm path if no menu is open |
| `Esc` | Close selector without saving |
| `Backspace` | Delete character before cursor |
| `Ctrl-A` | Move cursor to start of input |
| `Ctrl-E` | Move cursor to end of input |
| `Ctrl-D` | Delete character under cursor |
| `Ctrl-K` | Delete from cursor to end of input |
| `Ctrl-U` | Delete from start of input to cursor |
| `Ctrl-W` | Delete word before cursor (stops at `/`) |
| `Ctrl-L` | Clear entire input |

---

## esp-agent

`esp-agent` is a static library that adds live telemetry to ESP32 firmware. Link it in and it self-starts; no changes to `app_main` are required.

**Prerequisites**

esp-agent uses `uxTaskGetSystemState` for task list and CPU usage, which requires runtime stats collection to be enabled in your firmware's `sdkconfig` (or `sdkconfig.defaults`):

```
CONFIG_FREERTOS_GENERATE_RUN_TIME_STATS=y
```

This implicitly enables `CONFIG_FREERTOS_USE_TRACE_FACILITY`. Without it the firmware will fail to link with an undefined reference to `uxTaskGetSystemState`.

**Linking**

First build the library for your target (see [Development](#development)):

```
cargo xtask build agent --target xtensa-esp32s3-espidf   # adjust for your chip
```

C/C++ (ESP-IDF v5, CMake): see `examples/c/` for a complete working project. The key points for integrating into your own component: declare `REQUIRES nvs_flash esp_wifi esp_hw_support` and anchor five symbols with `--undefined` so `--gc-sections` does not drop them before the agent archive is processed. `_esp_agent_ctor` and `esp_chip_info` are always required; the other three (`esp_read_mac`, `esp_wifi_sta_get_ap_info`, `nvs_get_stats`) are only required when your app does not already use WiFi or NVS directly.

The `<triple>` for each chip: `xtensa-esp32-espidf`, `xtensa-esp32s2-espidf`, `xtensa-esp32s3-espidf`, `riscv32imc-unknown-none-elf` (C3/C2), `riscv32imac-unknown-none-elf` (C6/H2).

Rust: see `examples/rust/` for a complete working project using `esp-idf-sys`. The RISC-V targets use `riscv32imc-esp-espidf` (C3/C2) and `riscv32imac-esp-espidf` (C6/H2) rather than the bare-metal `none-elf` variants. To integrate into your own project, emit the linker directives from a `build.rs` and force the linker to include the constructor symbol:

```rust
fn main() {
    println!("cargo:rustc-link-search=/path/to/esp-tui/target/<triple>/release");
    println!("cargo:rustc-link-lib=static=esp_agent");
    println!("cargo:rustc-link-arg=-Wl,--undefined=_esp_agent_ctor");
}
```

Use an absolute path in `rustc-link-search`; a relative path resolves against the project root, not the esp-tui workspace. The `--undefined` flag is required because no Rust code references the constructor directly; without it the linker silently drops the archive.

**Optional configuration**

By default the agent samples every 1000 ms. Override from `app_main` before the scheduler starts. Output always goes to stdout (the ESP-IDF configured console).

C/C++:
```c
esp_agent_configure(500);  // 500 ms
```

Rust:
```rust
unsafe extern "C" {
    fn esp_agent_configure(interval_ms: u32);
}

fn app_main() {
    unsafe { esp_agent_configure(500); }
}
```

**Wire format**

The agent and esp-tui communicate through three ESP-IDF VERBOSE log line types under the tag `esp_agent`. These lines are valid standard serial output readable in any monitor; esp-tui additionally parses them to populate the System Inspector pane.

`start` is emitted once on boot:

```
V (123) esp_agent: start reason=poweron chip=esp32s3 cores=2 rev=1 mac=AA:BB:CC:DD:EE:FF flash=4194304
```

Fields: `reason` (reset cause: `poweron` `sw` `panic` `int_wdt` `task_wdt` `wdt` `brownout` `deepsleep` `ext` `unknown`), `chip` (model name), `cores`, `rev` (silicon revision), `mac` (WiFi station MAC, colon-separated uppercase hex), `flash` (flash size in bytes).

`parts` is emitted once on boot:

```
V (124) esp_agent: parts nvs:d:0x9000:24576,ota_0:a:0x10000:1572864
```

Comma-separated partition entries, each `label:type:0xoffset:size`. Type is `a` (app) or `d` (data). Offsets are lowercase hex.

The periodic telemetry line is emitted every sampling interval:

```
V (12345) esp_agent: heap=142336/327680 min=98304 frag=65536 iram=45056 psram=0 cpu=23,45 wifi=-65 nvs=45/512 tasks=main:R:3200:1,wifi_task:B:1856:5
```

Fields: `heap=free/total` (bytes), `min` (heap low-water mark), `frag` (largest contiguous free block), `iram` (internal SRAM free), `psram` (PSRAM free, `0` if absent), `cpu` (per-core usage %, comma-separated), `wifi` (RSSI in dBm, omitted if not connected), `nvs=used/total` entries (omitted if NVS not initialised), `tasks` as comma-separated `name:state:stack_hwm:priority` (state chars: `R`=running `r`=ready `B`=blocked `S`=suspended `D`=deleted).

**Building from source**

See the [Development](#development) section above.

---

## Roadmap

| Phase | Description | Status |
|---|---|---|
| 1 | Serial monitor MVP | Complete |
| 2 | Flash integration (espflash, progress bar, board info) | Complete |
| 3 | `esp-agent` embedded library + System Inspector pane | In progress |
| 4 | Polish | Planned |

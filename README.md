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

**Phase 3**

- `esp-agent`: a zero-dependency `no_std` static library you link into ESP32 firmware
- Auto-starts a FreeRTOS task on boot via an `.init_array` constructor; no changes to `app_main` required
- Emits heap, CPU, WiFi RSSI, NVS, and task-list telemetry as ESP-IDF VERBOSE log lines (tag `esp_agent`); parsed by esp-tui to populate the System Inspector pane, and readable in any serial monitor
- Optional override via `esp_agent_configure(interval_ms)` for custom sampling interval
- Builds a `.a` for all five ESP32 targets via `cargo xtask build agent` (ESP32, S2, S3, C3, C6)
- System Inspector pane with live heap gauges (free/total/low-water/largest block), per-core CPU bars with ASCII sparklines showing the last 60 samples, WiFi RSSI and channel, NVS usage, uptime, scrollable task table, and partition table viewer; board info section shows chip model, cores, silicon revision, reset reason, flash size, and MAC; graceful fallback messages when no device or agent is connected
- Pane focus system: `Tab` cycles between the Serial Monitor and System Inspector; `Ctrl-F` opens the filter popup with a live search bar for tag filtering

**Phase 4**

- Panic backtrace decoder: detects an ESP-IDF `Backtrace:` dump on the serial stream and resolves each address against the configured ELF's DWARF debug info via `addr2line`, showing function, file, and line per frame in a popup (`b`); an always-live ELF path box inside the popup points the decoder at a different ELF and re-resolves in place, without reflashing
- `esp-tui.toml` config file for port, baud, ELF path, buffer size, colors, and keybindings, with `~/.config/esp-tui/config.toml` as a global fallback and CLI flags taking priority
- Configurable keybindings via `[keys]` in the config file, with `vim` and `emacs` presets (`--preset` flag or `preset =` in config) and per-key overrides via `[keys.overrides]`
- Per-pane resizing: `Ctrl-←` / `Ctrl-→` moves the split divider, auto-switching focus when a pane collapses
- Log search: regex search bar in the filter popup, with matches highlighted inline, `n` / `N` to jump between matches, and invalid patterns shown in red
- macOS port filtering: only `tty.*` devices are offered by the port selector and auto-connect, since `cu.*` is not the correct interface for ESP32 serial communication
- `--pane <monitor|inspector>` CLI flag to open with only one pane visible
- `--elf <path>` CLI flag to pre-load an ELF for flashing and backtrace symbol resolution
- `--config <file>` CLI flag to point at a config file other than `./esp-tui.toml`

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

Then build pre-compiled static libraries for all five ESP32 targets:

```
cargo xtask build agent                                    # all targets
cargo xtask build agent --target xtensa-esp32s3-espidf    # one target
```

Produces `target/<triple>/release/libesp_agent.a` for each target. No environment setup is needed beyond running `espup install`; the xtask resolves the toolchain paths automatically.

**Examples**

Working reference projects live in `examples/c/`, `examples/rust/`, and `examples/panic/`. Each can be built with a single command from the repo root; the xtask builds the agent first and then the example:

```
cargo xtask build examples                                             # all three, all targets
cargo xtask build examples rust                                        # Rust only, all targets
cargo xtask build examples c                                           # C only, all targets
cargo xtask build examples panic                                       # panic demo only, all targets
cargo xtask build examples rust --target xtensa-esp32s3-espidf        # one target
```

Each command builds for all five ESP-IDF targets by default (ESP32, S2, S3, C3, C6). Pass a target triple as the second argument to build for a single chip. The xtask auto-detects the ESP-IDF installation at `~/.espressif/esp-idf/v5.3.1` for the C and panic examples; set `IDF_PATH` to override.

`examples/panic/` doesn't demonstrate `esp-agent`; it exists to exercise the panic backtrace decoder against a real device. It logs a message, waits 5 seconds, then deliberately dereferences a null pointer, producing a genuine `Guru Meditation Error` / `Backtrace:` dump on the serial output. Flash it, point esp-tui's ELF path at the build output (e.g. `examples/panic/build/esp32/esp_tui_panic_example.elf`), and connect to watch the decoder resolve the crash live.

**Devcontainer**

Opening the repo in a devcontainer installs all prerequisites automatically, including the Xtensa toolchain.

---

## Usage

```
esp-tui [OPTIONS]

Options:
  -p, --port <PORT>    Serial port to connect to
  -b, --baud <BAUD>    Serial baud rate (default: 115200)
      --pane <PANE>    Open with only one pane visible [monitor|inspector]
      --config <FILE>  Path to a config file (default: ./esp-tui.toml)
      --preset <NAME>  Keybinding preset: vim, emacs, or a path to a preset .toml file
      --elf <PATH>     ELF path to pre-fill the flash selector and resolve backtrace symbols
  -h, --help           Print help
```

**Examples**

```
esp-tui                           # auto-detect port
esp-tui --port /dev/ttyUSB0       # connect to a specific port
esp-tui --pane inspector          # open with only the System Inspector visible
esp-tui --preset vim              # use vim-style keybindings for this session
esp-tui --elf build/firmware.elf  # pre-load an ELF for flashing and backtrace symbols
```

---

## Configuration

esp-tui reads `esp-tui.toml` in the current directory, falling back to
`~/.config/esp-tui/config.toml` for global defaults. CLI flags override both.

See [`esp-tui.example.toml`](./esp-tui.example.toml) in the repo root for a
fully documented reference of every available key.

`[flash].elf_path` (or the equivalent `--elf` flag, which takes priority)
only seeds the initial ELF path at startup; neither triggers a flash. Two
places change the live path at runtime, with different side effects:
confirming the flash popup's selector (`f`, then `Enter`) updates the path
*and* starts a flash; confirming the backtrace popup's box (`b`, then
`Enter`) updates the same
path but only re-resolves the currently displayed backtrace, and never
flashes.

---

## Keybindings

All bindings below are defaults. Every action is remappable via `[keys]` in
`esp-tui.toml`; see [`esp-tui.example.toml`](./esp-tui.example.toml) for the
full reference.

| Key | Action |
|---|---|
| `c` | Connect / scan for ports |
| `d` | Disconnect |
| `f` | Open ELF path selector and flash to device |
| `e` | Erase flash (shows confirmation prompt) |
| `r` | Reset device (DTR/RTS) |
| `Tab` | Cycle focus between Serial Monitor and System Inspector panes |
| `Ctrl-←` / `Ctrl-→` | Move the split divider left / right; auto-switches focus when a pane collapses |
| `Ctrl-F` | Open / close filter popup |
| `Space` | Toggle filter item (inside filter popup) |
| `Ctrl-A` | Toggle all filter items (inside filter popup) |
| `↑` / `↓` | Scroll the focused pane up / down |
| `PgUp` / `PgDn` | Scroll the focused pane by 10 lines |
| `Ctrl-L` | Clear log buffer |
| `b` | Open / close the decoded panic backtrace popup (only when one has been captured) |
| `q` / `Esc` | Exit scroll mode, or quit |
| `Ctrl-C` | Quit |

**Presets**

Set `preset = "vim"` or `preset = "emacs"` under `[keys]`, or pass `--preset
vim` / `--preset emacs` on the command line (the CLI flag wins if both are
set), to switch to a familiar binding scheme. Presets replace the default
scroll and filter keys; all other defaults remain.

| Action | vim | emacs |
|---|---|---|
| Scroll up / down | `k` / `j` | `Ctrl-P` / `Ctrl-N` |
| Page up / down | `Ctrl-B` / `Ctrl-F` | `Alt-V` / `Ctrl-V` |
| Jump to top / bottom | `g` / `G` | `Alt-<` / `Alt->` |
| Open / close filter | `/` | `Ctrl-S` |
| Switch pane | `Ctrl-W` | |
| Cancel / quit prompt | | `Ctrl-G` |

Individual bindings can be overridden on top of a preset via `[keys.overrides]`.

**Filter popup** (type any character to search tags by name)

| Key | Action |
|---|---|
| Type characters | Narrow the tag list by substring match |
| `Backspace` | Remove last character from search |
| `↑` / `↓` | Move selection |
| `Space` | Toggle selected item |
| `Ctrl-A` | Toggle all items |
| `Esc` / filter key | Close popup |

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

**Backtrace popup** (active while the `b` popup is open; the ELF path box
is always live, there is no separate focus mode to enter)

| Key | Action |
|---|---|
| Type characters | Insert into the ELF path box |
| `Tab` | Tab-complete: auto-accept single match, extend to common prefix for multiple |
| `Shift-Tab` | Cycle completions backward |
| `←` / `→` | Move cursor left / right in the box |
| `Enter` | Accept highlighted completion, or (if no menu is open) load the typed path: updates the same ELF path the flash selector uses and re-resolves this backtrace's symbols in place, without flashing |
| `↑` / `↓` | Navigate the completion dropdown when one is open; otherwise scroll the frame list |
| `PgUp` / `PgDn` | Scroll the frame list (never moves into the box) |
| `Esc` | Close the popup; any unsaved edit to the box is discarded |
| `Backspace` | Delete character before cursor |
| `Ctrl-A` | Move cursor to start of box |
| `Ctrl-E` | Move cursor to end of box |
| `Ctrl-D` | Delete character under cursor |
| `Ctrl-K` | Delete from cursor to end of box |
| `Ctrl-U` | Delete from start of box to cursor |
| `Ctrl-W` | Delete word before cursor (stops at `/`) |
| `Ctrl-L` | Clear entire box |

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

The `<triple>` for each chip: `xtensa-esp32-espidf`, `xtensa-esp32s2-espidf`, `xtensa-esp32s3-espidf`, `riscv32imc-esp-espidf` (C3/C2), `riscv32imac-esp-espidf` (C6/H2).

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
V (123) esp_agent: start reason=poweron chip=esp32s3 cores=2 rev=1 mac=AA:BB:CC:DD:EE:FF flash=0x400000
```

Fields: `reason` (reset cause: `poweron` `sw` `panic` `int_wdt` `task_wdt` `wdt` `brownout` `deepsleep` `ext` `unknown`), `chip` (model name), `cores`, `rev` (silicon revision), `mac` (WiFi station MAC, colon-separated uppercase hex), `flash` (flash size in bytes).

`parts` is emitted once on boot:

```
V (124) esp_agent: parts nvs:d:0x9000:0x6000,ota_0:a:0x10000:0x180000
```

Comma-separated partition entries, each `label:type:0xoffset:0xsize`. Type is `a` (app) or `d` (data). Offsets and sizes are lowercase hex.

The periodic telemetry line is emitted every sampling interval:

```
V (12345) esp_agent: heap=142336/327680 min=98304 frag=65536 iram=45056 psram=0 cpu=23,45 wifi=-65 nvs=45/512 tasks=main:R:3200:1,wifi_task:B:1856:5
```

Fields: `heap=free/total` (bytes), `min` (heap low-water mark), `frag` (largest contiguous free block), `iram` (internal SRAM free), `psram` (PSRAM free, `0` if absent), `cpu` (per-core usage %, comma-separated), `wifi` (RSSI in dBm, omitted if not connected), `wifi_ch` (WiFi channel number, omitted if not connected), `nvs=used/total` entries (omitted if NVS not initialised), `tasks` as comma-separated `name:state:stack_hwm:priority` (state chars: `R`=running `r`=ready `B`=blocked `S`=suspended `D`=deleted).

**Building from source**

See the [Development](#development) section above.

---

## Roadmap

| Phase | Description | Status |
|---|---|---|
| 1 | Serial monitor MVP | Complete |
| 2 | Flash integration (espflash, progress bar, board info) | Complete |
| 3 | `esp-agent` embedded library + System Inspector pane | Complete |
| 4 | Polish | Planned |

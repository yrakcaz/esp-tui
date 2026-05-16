/// Maximum number of `FreeRTOS` tasks captured per telemetry frame.
pub(crate) const MAX_TASKS: usize = 32;

/// Maximum bytes in a `FreeRTOS` task name (including null terminator).
pub(crate) const MAX_TASK_NAME: usize = 16;

/// Maximum number of partition table entries captured per parts line.
pub(crate) const MAX_PARTITIONS: usize = 16;

/// Maximum bytes in an ESP-IDF partition label (including null terminator).
pub(crate) const MAX_PARTITION_LABEL: usize = 17;

/// Maximum byte length of a formatted telemetry or start line.
pub(crate) const MAX_LINE: usize = 768;

/// Maximum byte length of a formatted partition line.
pub(crate) const MAX_PARTS_LINE: usize = 512;

/// `FreeRTOS` task scheduling state.
#[derive(Clone, Copy)]
pub(crate) enum TaskState {
    Running,
    Ready,
    Blocked,
    Suspended,
    Deleted,
}

/// One `FreeRTOS` task entry for inclusion in a telemetry frame.
#[derive(Clone, Copy)]
pub(crate) struct TaskInfo {
    /// Task name bytes; only `name[..name_len]` is valid.
    pub(crate) name: [u8; MAX_TASK_NAME],
    /// Number of valid bytes in `name`.
    pub(crate) name_len: usize,
    /// Current scheduling state.
    pub(crate) state: TaskState,
    /// Stack high-water mark in bytes (minimum free stack ever observed).
    pub(crate) stack_hwm: u32,
    /// Current task priority.
    pub(crate) priority: u32,
}

/// A snapshot of device metrics sampled in one agent task iteration.
pub(crate) struct TelemetryFrame {
    /// Tick count at the time of sampling, in milliseconds.
    pub(crate) timestamp_ms: u32,
    /// Default-heap free bytes (`MALLOC_CAP_DEFAULT`).
    pub(crate) heap_free: u32,
    /// Default-heap total bytes.
    pub(crate) heap_total: u32,
    /// Minimum free heap ever observed (low-water mark).
    pub(crate) heap_min_free: u32,
    /// Largest contiguous free block in the default heap (fragmentation indicator).
    pub(crate) heap_frag: u32,
    /// Internal SRAM free bytes (`MALLOC_CAP_INTERNAL`).
    pub(crate) heap_iram: u32,
    /// PSRAM free bytes; `0` if no PSRAM is present or configured.
    pub(crate) heap_psram: u32,
    /// Number of cores whose usage is reported in `cpu_usage`.
    pub(crate) cpu_cores: u8,
    /// Per-core CPU usage as an integer percentage; index 0 is core 0.
    pub(crate) cpu_usage: [u8; 2],
    /// `WiFi` station RSSI in dBm; `None` if not connected.
    pub(crate) wifi_rssi: Option<i8>,
    /// NVS used entry count; `None` if NVS is not initialised.
    pub(crate) nvs_used: Option<u32>,
    /// NVS total entry count; `None` if NVS is not initialised.
    pub(crate) nvs_total: Option<u32>,
    /// Number of valid entries in `tasks`.
    pub(crate) task_count: usize,
    /// Task entries; only `tasks[..task_count]` is valid.
    pub(crate) tasks: [TaskInfo; MAX_TASKS],
}

/// ESP32 reset reason.
#[derive(Clone, Copy)]
pub(crate) enum ResetReason {
    PowerOn,
    Software,
    Panic,
    IntWatchdog,
    TaskWatchdog,
    Watchdog,
    Brownout,
    DeepSleep,
    External,
    Unknown,
}

/// Startup metadata sent once in the agent start line.
#[derive(Clone, Copy)]
pub(crate) struct StartupInfo {
    /// Reason the device last reset.
    pub(crate) reason: ResetReason,
    /// Chip model name as ASCII bytes; only `chip_name[..chip_name_len]` is valid.
    pub(crate) chip_name: [u8; 16],
    /// Number of valid bytes in `chip_name`.
    pub(crate) chip_name_len: usize,
    /// Number of CPU cores.
    pub(crate) cores: u8,
    /// Silicon revision number.
    pub(crate) revision: u16,
    /// `WiFi` station MAC address bytes (from `esp_read_mac`).
    pub(crate) mac: [u8; 6],
    /// Default flash chip size in bytes (from `esp_flash_get_size`).
    pub(crate) flash_size: u32,
}

/// One partition table entry for inclusion in a parts line.
#[derive(Clone, Copy)]
pub(crate) struct PartitionEntry {
    /// Partition label bytes; only `label[..label_len]` is valid.
    pub(crate) label: [u8; MAX_PARTITION_LABEL],
    /// Number of valid bytes in `label`.
    pub(crate) label_len: usize,
    /// Raw partition type byte: `0` = app, `1` = data.
    pub(crate) type_: u8,
    /// Partition start address in flash.
    pub(crate) offset: u32,
    /// Partition size in bytes.
    pub(crate) size: u32,
}

/// A zeroed-out [`TaskInfo`] used to initialise fixed-size task arrays.
#[cfg(not(test))]
pub(crate) const EMPTY_TASK_INFO: TaskInfo = TaskInfo {
    name: [0; MAX_TASK_NAME],
    name_len: 0,
    state: TaskState::Blocked,
    stack_hwm: 0,
    priority: 0,
};

/// Converts a `FreeRTOS` task state integer to [`TaskState`].
///
/// # Arguments
///
/// * `state` - The integer value from `TaskStatus.current_state`.
///
/// # Returns
///
/// The matching `TaskState` variant; out-of-range values map to `Deleted`.
pub(crate) fn task_state_from_u32(state: u32) -> TaskState {
    match state {
        0 => TaskState::Running,
        1 => TaskState::Ready,
        2 => TaskState::Blocked,
        3 => TaskState::Suspended,
        _ => TaskState::Deleted,
    }
}

fn write_byte(b: u8, buf: &mut [u8], pos: &mut usize) -> bool {
    if *pos < buf.len() {
        buf[*pos] = b;
        *pos += 1;
        true
    } else {
        false
    }
}

fn write_str(s: &[u8], buf: &mut [u8], pos: &mut usize) -> bool {
    s.iter().all(|&b| write_byte(b, buf, pos))
}

fn write_u32(val: u32, buf: &mut [u8], pos: &mut usize) -> bool {
    if val == 0 {
        write_byte(b'0', buf, pos)
    } else {
        let mut tmp = [0u8; 10];
        let mut len = 0usize;
        let mut v = val;
        while v > 0 {
            tmp[len] = b'0' + (v % 10) as u8;
            len += 1;
            v /= 10;
        }
        (0..len).rev().all(|i| write_byte(tmp[i], buf, pos))
    }
}

fn write_i8(val: i8, buf: &mut [u8], pos: &mut usize) -> bool {
    if val < 0 {
        write_byte(b'-', buf, pos)
            && write_u32(u32::from(val.unsigned_abs()), buf, pos)
    } else {
        write_u32(u32::from(val.unsigned_abs()), buf, pos)
    }
}

fn write_u32_hex(val: u32, buf: &mut [u8], pos: &mut usize) -> bool {
    if val == 0 {
        write_byte(b'0', buf, pos)
    } else {
        const HEX: &[u8] = b"0123456789abcdef";
        let mut tmp = [0u8; 8];
        let mut len = 0usize;
        let mut v = val;
        while v > 0 {
            tmp[len] = HEX[(v & 0xF) as usize];
            len += 1;
            v >>= 4;
        }
        (0..len).rev().all(|i| write_byte(tmp[i], buf, pos))
    }
}

fn task_state_char(state: TaskState) -> u8 {
    match state {
        TaskState::Running => b'R',
        TaskState::Ready => b'r',
        TaskState::Blocked => b'B',
        TaskState::Suspended => b'S',
        TaskState::Deleted => b'D',
    }
}

fn partition_type_char(type_: u8) -> u8 {
    match type_ {
        0 => b'a',
        1 => b'd',
        _ => b'?',
    }
}

fn reset_reason_str(reason: ResetReason) -> &'static [u8] {
    match reason {
        ResetReason::PowerOn => b"poweron",
        ResetReason::Software => b"sw",
        ResetReason::Panic => b"panic",
        ResetReason::IntWatchdog => b"int_wdt",
        ResetReason::TaskWatchdog => b"task_wdt",
        ResetReason::Watchdog => b"wdt",
        ResetReason::Brownout => b"brownout",
        ResetReason::DeepSleep => b"deepsleep",
        ResetReason::External => b"ext",
        ResetReason::Unknown => b"unknown",
    }
}

/// Formats a startup line into `out`.
///
/// Output: `V ({timestamp_ms}) esp_agent: start reason=R chip=C cores=N
/// rev=V mac=XX:XX:XX:XX:XX:XX flash=0xF\r\n`
///
/// # Arguments
///
/// * `timestamp_ms` - Current tick count in milliseconds.
/// * `info`         - Startup metadata collected from ESP-IDF APIs.
/// * `out`          - Output buffer; must be at least [`MAX_LINE`] bytes.
///
/// # Returns
///
/// Number of bytes written, or `None` if `out` is too short.
#[must_use]
pub(crate) fn format_start_line(
    timestamp_ms: u32,
    info: &StartupInfo,
    out: &mut [u8],
) -> Option<usize> {
    let mut pos = 0usize;
    let ok = write_str(b"V (", out, &mut pos)
        && write_u32(timestamp_ms, out, &mut pos)
        && write_str(b") esp_agent: start reason=", out, &mut pos)
        && write_str(reset_reason_str(info.reason), out, &mut pos)
        && write_str(b" chip=", out, &mut pos)
        && write_str(&info.chip_name[..info.chip_name_len], out, &mut pos)
        && write_str(b" cores=", out, &mut pos)
        && write_u32(u32::from(info.cores), out, &mut pos)
        && write_str(b" rev=", out, &mut pos)
        && write_u32(u32::from(info.revision), out, &mut pos)
        && write_str(b" mac=", out, &mut pos)
        && {
            const HEX: &[u8] = b"0123456789ABCDEF";
            info.mac.iter().enumerate().all(|(i, &byte)| {
                (i == 0 || write_byte(b':', out, &mut pos))
                    && write_byte(HEX[usize::from(byte >> 4)], out, &mut pos)
                    && write_byte(HEX[usize::from(byte & 0xF)], out, &mut pos)
            })
        }
        && write_str(b" flash=0x", out, &mut pos)
        && write_u32_hex(info.flash_size, out, &mut pos)
        && write_str(b"\r\n", out, &mut pos);
    ok.then_some(pos)
}

/// Formats a periodic telemetry line into `out`.
///
/// Output: `V ({ms}) esp_agent: heap=F/T min=M frag=G iram=I psram=P
/// cpu=C0[,C1] [wifi=R] [nvs=U/N] tasks=name:S:hwm:prio,...\r\n`
///
/// The `wifi=` field is omitted when `frame.wifi_rssi` is `None`.
/// The `nvs=` field is omitted when either `nvs_used` or `nvs_total`
/// is `None`.
///
/// # Arguments
///
/// * `frame` - Telemetry snapshot to format.
/// * `out`   - Output buffer; must be at least [`MAX_LINE`] bytes.
///
/// # Returns
///
/// Number of bytes written, or `None` if `out` is too short.
#[must_use]
pub(crate) fn format_telemetry_line(
    frame: &TelemetryFrame,
    out: &mut [u8],
) -> Option<usize> {
    let mut pos = 0usize;
    let task_count = frame.task_count.min(MAX_TASKS);
    let ok = write_str(b"V (", out, &mut pos)
        && write_u32(frame.timestamp_ms, out, &mut pos)
        && write_str(b") esp_agent: heap=", out, &mut pos)
        && write_u32(frame.heap_free, out, &mut pos)
        && write_byte(b'/', out, &mut pos)
        && write_u32(frame.heap_total, out, &mut pos)
        && write_str(b" min=", out, &mut pos)
        && write_u32(frame.heap_min_free, out, &mut pos)
        && write_str(b" frag=", out, &mut pos)
        && write_u32(frame.heap_frag, out, &mut pos)
        && write_str(b" iram=", out, &mut pos)
        && write_u32(frame.heap_iram, out, &mut pos)
        && write_str(b" psram=", out, &mut pos)
        && write_u32(frame.heap_psram, out, &mut pos)
        && write_str(b" cpu=", out, &mut pos)
        && write_u32(u32::from(frame.cpu_usage[0]), out, &mut pos)
        && (frame.cpu_cores < 2
            || (write_byte(b',', out, &mut pos)
                && write_u32(u32::from(frame.cpu_usage[1]), out, &mut pos)))
        && frame.wifi_rssi.is_none_or(|rssi| {
            write_str(b" wifi=", out, &mut pos) && write_i8(rssi, out, &mut pos)
        })
        && frame
            .nvs_used
            .zip(frame.nvs_total)
            .is_none_or(|(used, total)| {
                write_str(b" nvs=", out, &mut pos)
                    && write_u32(used, out, &mut pos)
                    && write_byte(b'/', out, &mut pos)
                    && write_u32(total, out, &mut pos)
            })
        && write_str(b" tasks=", out, &mut pos)
        && frame.tasks[..task_count].iter().enumerate().all(|(i, t)| {
            (i == 0 || write_byte(b',', out, &mut pos))
                && write_str(&t.name[..t.name_len], out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_byte(task_state_char(t.state), out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_u32(t.stack_hwm, out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_u32(t.priority, out, &mut pos)
        })
        && write_str(b"\r\n", out, &mut pos);
    ok.then_some(pos)
}

/// Formats a partition table line into `out`.
///
/// Output: `V ({timestamp_ms}) esp_agent: parts
/// label:t:0xoffset:0xsize,...\r\n`
///
/// Type char `t` is `a` for app partitions and `d` for data partitions.
/// Partition offsets and sizes are written as lowercase hex with a `0x` prefix.
/// An empty `parts` slice produces a valid line with no entries.
///
/// # Arguments
///
/// * `timestamp_ms` - Current tick count in milliseconds.
/// * `parts`        - Partition entries to format.
/// * `out`          - Output buffer; must be at least [`MAX_PARTS_LINE`] bytes.
///
/// # Returns
///
/// Number of bytes written, or `None` if `out` is too short.
#[must_use]
pub(crate) fn format_parts_line(
    timestamp_ms: u32,
    parts: &[PartitionEntry],
    out: &mut [u8],
) -> Option<usize> {
    let mut pos = 0usize;
    let ok = write_str(b"V (", out, &mut pos)
        && write_u32(timestamp_ms, out, &mut pos)
        && write_str(b") esp_agent: parts", out, &mut pos)
        && parts.iter().enumerate().all(|(i, part)| {
            write_byte(if i == 0 { b' ' } else { b',' }, out, &mut pos)
                && write_str(&part.label[..part.label_len], out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_byte(partition_type_char(part.type_), out, &mut pos)
                && write_str(b":0x", out, &mut pos)
                && write_u32_hex(part.offset, out, &mut pos)
                && write_str(b":0x", out, &mut pos)
                && write_u32_hex(part.size, out, &mut pos)
        })
        && write_str(b"\r\n", out, &mut pos);
    ok.then_some(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_startup_info(reason: ResetReason) -> StartupInfo {
        let mut chip_name = [0u8; 16];
        chip_name[..7].copy_from_slice(b"esp32s3");
        StartupInfo {
            reason,
            chip_name,
            chip_name_len: 7,
            cores: 2,
            revision: 1,
            mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            flash_size: 4_194_304,
        }
    }

    const EMPTY_TASK: TaskInfo = TaskInfo {
        name: [0; MAX_TASK_NAME],
        name_len: 0,
        state: TaskState::Blocked,
        stack_hwm: 0,
        priority: 0,
    };

    fn make_frame(
        cpu_cores: u8,
        wifi_rssi: Option<i8>,
        nvs: Option<(u32, u32)>,
    ) -> TelemetryFrame {
        TelemetryFrame {
            timestamp_ms: 12345,
            heap_free: 142_336,
            heap_total: 327_680,
            heap_min_free: 98_304,
            heap_frag: 65_536,
            heap_iram: 45_056,
            heap_psram: 0,
            cpu_cores,
            cpu_usage: [23, 45],
            wifi_rssi,
            nvs_used: nvs.map(|(u, _)| u),
            nvs_total: nvs.map(|(_, t)| t),
            task_count: 0,
            tasks: [EMPTY_TASK; MAX_TASKS],
        }
    }

    fn output_str(buf: &[u8], n: usize) -> &str {
        core::str::from_utf8(&buf[..n]).unwrap()
    }

    #[test]
    fn format_start_line_structure() {
        let info = make_startup_info(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(123, &info, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("esp_agent: start reason="), "{s}");
        assert!(s.contains("chip="), "{s}");
        assert!(s.contains("cores="), "{s}");
        assert!(s.contains("rev="), "{s}");
    }

    #[test]
    fn format_start_line_ends_with_crlf() {
        let info = make_startup_info(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(0, &info, &mut buf).unwrap();
        assert!(buf[..n].ends_with(b"\r\n"));
    }

    #[test]
    fn format_start_line_no_binary() {
        let info = make_startup_info(ResetReason::Panic);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(999, &info, &mut buf).unwrap();
        assert!(buf[..n]
            .iter()
            .all(|&b| b == b'\r' || b == b'\n' || (0x20..=0x7E).contains(&b)));
    }

    #[test]
    fn format_start_line_reason_strings() {
        let cases = [
            (ResetReason::PowerOn, "reason=poweron"),
            (ResetReason::Software, "reason=sw"),
            (ResetReason::Panic, "reason=panic"),
            (ResetReason::IntWatchdog, "reason=int_wdt"),
            (ResetReason::TaskWatchdog, "reason=task_wdt"),
            (ResetReason::Watchdog, "reason=wdt"),
            (ResetReason::Brownout, "reason=brownout"),
            (ResetReason::DeepSleep, "reason=deepsleep"),
            (ResetReason::External, "reason=ext"),
            (ResetReason::Unknown, "reason=unknown"),
        ];
        for (reason, expected) in cases {
            let info = make_startup_info(reason);
            let mut buf = [0u8; MAX_LINE];
            let n = format_start_line(0, &info, &mut buf).unwrap();
            let s = output_str(&buf, n);
            assert!(s.contains(expected), "expected {expected:?} in {s:?}");
        }
    }

    #[test]
    fn format_start_line_chip_names() {
        for (chip_bytes, expected) in [
            (b"esp32\0\0\0\0\0\0\0\0\0\0\0" as &[u8; 16], "chip=esp32"),
            (b"esp32s3\0\0\0\0\0\0\0\0\0" as &[u8; 16], "chip=esp32s3"),
        ] {
            let info = StartupInfo {
                reason: ResetReason::PowerOn,
                chip_name: *chip_bytes,
                chip_name_len: expected.len() - "chip=".len(),
                cores: 1,
                revision: 0,
                mac: [0; 6],
                flash_size: 0,
            };
            let mut buf = [0u8; MAX_LINE];
            let n = format_start_line(0, &info, &mut buf).unwrap();
            let s = output_str(&buf, n);
            assert!(s.contains(expected), "{s}");
        }
    }

    #[test]
    fn format_start_line_mac() {
        let info = make_startup_info(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(0, &info, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("mac=AA:BB:CC:DD:EE:FF"), "{s}");
    }

    #[test]
    fn format_start_line_flash_size() {
        let info = make_startup_info(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(0, &info, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("flash=0x400000"), "{s}");
    }

    #[test]
    fn format_telemetry_line_structure() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("esp_agent: heap="), "{s}");
        assert!(s.contains("min="), "{s}");
        assert!(s.contains("frag="), "{s}");
        assert!(s.contains("iram="), "{s}");
        assert!(s.contains("psram="), "{s}");
        assert!(s.contains("cpu="), "{s}");
        assert!(s.contains("tasks="), "{s}");
    }

    #[test]
    fn format_telemetry_line_ends_with_crlf() {
        let frame = make_frame(1, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        assert!(buf[..n].ends_with(b"\r\n"));
    }

    #[test]
    fn format_telemetry_line_no_binary() {
        let frame = make_frame(2, Some(-65), Some((45, 512)));
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        assert!(buf[..n]
            .iter()
            .all(|&b| b == b'\r' || b == b'\n' || (0x20..=0x7E).contains(&b)));
    }

    #[test]
    fn format_telemetry_line_heap_fields() {
        let frame = make_frame(1, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("heap=142336/327680"), "{s}");
        assert!(s.contains("min=98304"), "{s}");
        assert!(s.contains("frag=65536"), "{s}");
        assert!(s.contains("iram=45056"), "{s}");
        assert!(s.contains("psram=0"), "{s}");
    }

    #[test]
    fn format_telemetry_line_cpu_single_core() {
        let frame = make_frame(1, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("cpu=23 "), "{s}");
        assert!(!s.contains("cpu=23,"), "{s}");
    }

    #[test]
    fn format_telemetry_line_cpu_dual_core() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("cpu=23,45"), "{s}");
    }

    #[test]
    fn format_telemetry_line_wifi_present() {
        let frame = make_frame(2, Some(-65), None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("wifi=-65"), "{s}");
    }

    #[test]
    fn format_telemetry_line_wifi_absent() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(!s.contains("wifi="), "{s}");
    }

    #[test]
    fn format_telemetry_line_nvs_present() {
        let frame = make_frame(2, None, Some((45, 512)));
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("nvs=45/512"), "{s}");
    }

    #[test]
    fn format_telemetry_line_nvs_absent() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(!s.contains("nvs="), "{s}");
    }

    #[test]
    fn format_telemetry_line_tasks_roundtrip() {
        let mut tasks = [EMPTY_TASK; MAX_TASKS];
        tasks[0] = TaskInfo {
            name: *b"main\0\0\0\0\0\0\0\0\0\0\0\0",
            name_len: 4,
            state: TaskState::Running,
            stack_hwm: 3200,
            priority: 1,
        };
        tasks[1] = TaskInfo {
            name: *b"wifi_task\0\0\0\0\0\0\0",
            name_len: 9,
            state: TaskState::Blocked,
            stack_hwm: 1856,
            priority: 5,
        };
        let frame = TelemetryFrame {
            task_count: 2,
            tasks,
            ..make_frame(2, None, None)
        };
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("main:R:3200:1"), "{s}");
        assert!(s.contains("wifi_task:B:1856:5"), "{s}");
    }

    #[test]
    fn format_telemetry_line_empty_tasks() {
        let frame = make_frame(1, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.ends_with("tasks=\r\n"), "{s}");
    }

    #[test]
    fn format_telemetry_line_max_tasks() {
        let mut tasks = [EMPTY_TASK; MAX_TASKS];
        for (i, t) in tasks.iter_mut().enumerate() {
            t.name[0] = b'a' + u8::try_from(i % 26).unwrap();
            t.name_len = 1;
            t.state = TaskState::Ready;
            t.stack_hwm = 1024;
            t.priority = 2;
        }
        let frame = TelemetryFrame {
            task_count: MAX_TASKS,
            tasks,
            ..make_frame(2, None, None)
        };
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        assert!(
            n < MAX_LINE,
            "32 tasks exceeded MAX_LINE: {n} >= {MAX_LINE}"
        );
        let s = output_str(&buf, n);
        assert!(s.contains("a:r:1024:2"), "{s}");
        assert!(s.contains("b:r:1024:2"), "{s}");
        assert!(s.contains("z:r:1024:2"), "{s}");
    }

    #[test]
    fn format_start_line_buffer_too_small() {
        let info = make_startup_info(ResetReason::PowerOn);
        let mut buf = [0u8; 4];
        assert_eq!(format_start_line(0, &info, &mut buf), None);
    }

    #[test]
    fn format_telemetry_line_buffer_too_small() {
        let frame = make_frame(1, None, None);
        let mut buf = [0u8; 4];
        assert_eq!(format_telemetry_line(&frame, &mut buf), None);
    }

    #[test]
    fn format_parts_line_buffer_too_small() {
        let mut buf = [0u8; 4];
        assert_eq!(format_parts_line(0, &[], &mut buf), None);
    }

    #[test]
    fn format_parts_line_prefix() {
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(123, &[], &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.starts_with("V ("), "{s}");
        assert!(s.contains("esp_agent: parts"), "{s}");
    }

    #[test]
    fn format_parts_line_ends_with_crlf() {
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[], &mut buf).unwrap();
        assert!(buf[..n].ends_with(b"\r\n"));
    }

    #[test]
    fn format_parts_line_no_binary() {
        let mut label = [0u8; MAX_PARTITION_LABEL];
        label[..3].copy_from_slice(b"nvs");
        let part = PartitionEntry {
            label,
            label_len: 3,
            type_: 1,
            offset: 0x9000,
            size: 24576,
        };
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[part], &mut buf).unwrap();
        assert!(buf[..n]
            .iter()
            .all(|&b| b == b'\r' || b == b'\n' || (0x20..=0x7E).contains(&b)));
    }

    #[test]
    fn format_parts_line_fields_roundtrip() {
        let mut label = [0u8; MAX_PARTITION_LABEL];
        label[..5].copy_from_slice(b"ota_0");
        let part = PartitionEntry {
            label,
            label_len: 5,
            type_: 0,
            offset: 0x10000,
            size: 1_572_864,
        };
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[part], &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("ota_0:a:0x10000:0x180000"), "{s}");
    }

    #[test]
    fn format_parts_line_app_type() {
        let mut label = [0u8; MAX_PARTITION_LABEL];
        label[..4].copy_from_slice(b"app0");
        let part = PartitionEntry {
            label,
            label_len: 4,
            type_: 0,
            offset: 0x10000,
            size: 1024,
        };
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[part], &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("app0:a:"), "{s}");
    }

    #[test]
    fn format_parts_line_data_type() {
        let mut label = [0u8; MAX_PARTITION_LABEL];
        label[..3].copy_from_slice(b"nvs");
        let part = PartitionEntry {
            label,
            label_len: 3,
            type_: 1,
            offset: 0x9000,
            size: 24576,
        };
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[part], &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.contains("nvs:d:"), "{s}");
    }

    #[test]
    fn format_parts_line_empty() {
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[], &mut buf).unwrap();
        let s = output_str(&buf, n);
        assert!(s.ends_with("parts\r\n"), "{s}");
    }

    #[test]
    fn format_parts_line_max_partitions() {
        let parts: [PartitionEntry; MAX_PARTITIONS] = core::array::from_fn(|i| {
            let mut label = [0u8; MAX_PARTITION_LABEL];
            label[0] = b'p';
            label[1] = b'0' + u8::try_from(i).unwrap();
            PartitionEntry {
                label,
                label_len: 2,
                type_: u8::try_from(i % 2).unwrap(),
                offset: u32::try_from(i).unwrap() * 0x1_0000,
                size: 65536,
            }
        });
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &parts, &mut buf).unwrap();
        assert!(
            n < MAX_PARTS_LINE,
            "16 partitions exceeded MAX_PARTS_LINE: {n}"
        );
    }

    #[test]
    fn write_u32_zero() {
        let mut buf = [0u8; 16];
        let mut pos = 0;
        assert!(write_u32(0, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"0");
    }

    #[test]
    fn write_u32_max() {
        let mut buf = [0u8; 16];
        let mut pos = 0;
        assert!(write_u32(u32::MAX, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"4294967295");
    }

    #[test]
    fn write_u32_known() {
        let mut buf = [0u8; 16];
        let mut pos = 0;
        assert!(write_u32(12345, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"12345");
    }

    #[test]
    fn write_u32_hex_known() {
        let mut buf = [0u8; 16];
        let mut pos = 0;
        assert!(write_u32_hex(0x10000, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"10000");
    }

    #[test]
    fn write_u32_hex_zero() {
        let mut buf = [0u8; 16];
        let mut pos = 0;
        assert!(write_u32_hex(0, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"0");
    }

    #[test]
    fn write_i8_positive() {
        let mut buf = [0u8; 8];
        let mut pos = 0;
        assert!(write_i8(23, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"23");
    }

    #[test]
    fn write_i8_negative() {
        let mut buf = [0u8; 8];
        let mut pos = 0;
        assert!(write_i8(-65, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"-65");
    }

    #[test]
    fn task_state_char_all() {
        assert_eq!(task_state_char(TaskState::Running), b'R');
        assert_eq!(task_state_char(TaskState::Ready), b'r');
        assert_eq!(task_state_char(TaskState::Blocked), b'B');
        assert_eq!(task_state_char(TaskState::Suspended), b'S');
        assert_eq!(task_state_char(TaskState::Deleted), b'D');
    }

    #[test]
    fn partition_type_char_known() {
        assert_eq!(partition_type_char(0), b'a');
        assert_eq!(partition_type_char(1), b'd');
        assert_eq!(partition_type_char(255), b'?');
    }

    #[test]
    fn task_state_from_u32_all() {
        assert!(matches!(task_state_from_u32(0), TaskState::Running));
        assert!(matches!(task_state_from_u32(1), TaskState::Ready));
        assert!(matches!(task_state_from_u32(2), TaskState::Blocked));
        assert!(matches!(task_state_from_u32(3), TaskState::Suspended));
        assert!(matches!(task_state_from_u32(4), TaskState::Deleted));
        assert!(matches!(task_state_from_u32(99), TaskState::Deleted));
    }
}

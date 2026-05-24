use crate::{Frame, PartType, Partition, ResetReason, Startup, TaskState};

fn write_byte(b: u8, buf: &mut [u8], pos: &mut usize) -> bool {
    if *pos < buf.len() {
        buf[*pos] = b;
        *pos += 1;
        true
    } else {
        false
    }
}

fn write_bytes(s: &[u8], buf: &mut [u8], pos: &mut usize) -> bool {
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

fn write_i32(val: i32, buf: &mut [u8], pos: &mut usize) -> bool {
    if val < 0 {
        write_byte(b'-', buf, pos) && write_u32(val.unsigned_abs(), buf, pos)
    } else {
        write_u32(val.unsigned_abs(), buf, pos)
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

fn partition_type_char(part_type: PartType) -> u8 {
    match part_type {
        PartType::App => b'a',
        PartType::Data => b'd',
        PartType::Unknown => b'?',
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
/// * `startup`      - Startup metadata.
/// * `out`          - Output buffer; must be at least [`crate::MAX_LINE`] bytes.
///
/// # Returns
///
/// Number of bytes written, or `None` if `out` is too short.
#[must_use]
pub fn format_start_line(
    timestamp_ms: u32,
    startup: &Startup,
    out: &mut [u8],
) -> Option<usize> {
    let mut pos = 0usize;
    let ok = write_bytes(b"V (", out, &mut pos)
        && write_u32(timestamp_ms, out, &mut pos)
        && write_bytes(b") esp_agent: start reason=", out, &mut pos)
        && write_bytes(reset_reason_str(startup.reason), out, &mut pos)
        && write_bytes(b" chip=", out, &mut pos)
        && write_bytes(startup.chip.as_bytes(), out, &mut pos)
        && write_bytes(b" cores=", out, &mut pos)
        && write_u32(u32::from(startup.cores), out, &mut pos)
        && write_bytes(b" rev=", out, &mut pos)
        && write_u32(u32::from(startup.revision), out, &mut pos)
        && write_bytes(b" mac=", out, &mut pos)
        && {
            const HEX: &[u8] = b"0123456789ABCDEF";
            startup.mac.iter().enumerate().all(|(i, &byte)| {
                (i == 0 || write_byte(b':', out, &mut pos))
                    && write_byte(HEX[usize::from(byte >> 4)], out, &mut pos)
                    && write_byte(HEX[usize::from(byte & 0xF)], out, &mut pos)
            })
        }
        && write_bytes(b" flash=0x", out, &mut pos)
        && write_u32_hex(startup.flash_size, out, &mut pos)
        && write_bytes(b"\r\n", out, &mut pos);
    ok.then_some(pos)
}

/// Formats a periodic telemetry line into `out`.
///
/// Output: `V ({ms}) esp_agent: heap=F/T min=M frag=G iram=I psram=P
/// cpu=C0[,C1] [wifi=R] [nvs=U/N] tasks=name:S:hwm:prio,...\r\n`
///
/// The `wifi=` field is omitted when `frame.wifi_rssi` is `None`.
/// The `nvs=` field is omitted when `frame.nvs` is `None`.
///
/// # Arguments
///
/// * `frame` - Telemetry snapshot to format.
/// * `out`   - Output buffer; must be at least [`crate::MAX_LINE`] bytes.
///
/// # Returns
///
/// Number of bytes written, or `None` if `out` is too short.
#[must_use]
pub fn format_telemetry_line(frame: &Frame, out: &mut [u8]) -> Option<usize> {
    let mut pos = 0usize;
    let ok = write_bytes(b"V (", out, &mut pos)
        && write_u32(frame.timestamp_ms, out, &mut pos)
        && write_bytes(b") esp_agent: heap=", out, &mut pos)
        && write_u32(frame.heap_free, out, &mut pos)
        && write_byte(b'/', out, &mut pos)
        && write_u32(frame.heap_total, out, &mut pos)
        && write_bytes(b" min=", out, &mut pos)
        && write_u32(frame.heap_min_free, out, &mut pos)
        && write_bytes(b" frag=", out, &mut pos)
        && write_u32(frame.heap_frag, out, &mut pos)
        && write_bytes(b" iram=", out, &mut pos)
        && write_u32(frame.heap_iram, out, &mut pos)
        && write_bytes(b" psram=", out, &mut pos)
        && write_u32(frame.heap_psram, out, &mut pos)
        && write_bytes(b" cpu=", out, &mut pos)
        && write_u32(
            u32::from(frame.cpu_usage.first().copied().unwrap_or(0)),
            out,
            &mut pos,
        )
        && (frame.cpu_usage.len() < 2
            || (write_byte(b',', out, &mut pos)
                && write_u32(
                    u32::from(frame.cpu_usage.get(1).copied().unwrap_or(0)),
                    out,
                    &mut pos,
                )))
        && frame.wifi_rssi.is_none_or(|rssi| {
            write_bytes(b" wifi=", out, &mut pos) && write_i32(rssi, out, &mut pos)
        })
        && frame.nvs.is_none_or(|(used, total)| {
            write_bytes(b" nvs=", out, &mut pos)
                && write_u32(used, out, &mut pos)
                && write_byte(b'/', out, &mut pos)
                && write_u32(total, out, &mut pos)
        })
        && write_bytes(b" tasks=", out, &mut pos)
        && frame.tasks.iter().enumerate().all(|(i, t)| {
            (i == 0 || write_byte(b',', out, &mut pos))
                && write_bytes(t.name.as_bytes(), out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_byte(task_state_char(t.state), out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_u32(t.hwm, out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_u32(t.priority, out, &mut pos)
        })
        && write_bytes(b"\r\n", out, &mut pos);
    ok.then_some(pos)
}

/// Formats a partition table line into `out`.
///
/// Output: `V ({timestamp_ms}) esp_agent: parts
/// label:t:0xoffset:0xsize,...\r\n`
///
/// Type char `t` is `a` for app partitions, `d` for data partitions, and `?`
/// for unknown types.
///
/// # Arguments
///
/// * `timestamp_ms` - Current tick count in milliseconds.
/// * `parts`        - Partition entries to format.
/// * `out`          - Output buffer; must be at least [`crate::MAX_PARTS_LINE`] bytes.
///
/// # Returns
///
/// Number of bytes written, or `None` if `out` is too short.
#[must_use]
pub fn format_parts_line(
    timestamp_ms: u32,
    parts: &[Partition],
    out: &mut [u8],
) -> Option<usize> {
    let mut pos = 0usize;
    let ok = write_bytes(b"V (", out, &mut pos)
        && write_u32(timestamp_ms, out, &mut pos)
        && write_bytes(b") esp_agent: parts", out, &mut pos)
        && parts.iter().enumerate().all(|(i, part)| {
            write_byte(if i == 0 { b' ' } else { b',' }, out, &mut pos)
                && write_bytes(part.label.as_bytes(), out, &mut pos)
                && write_byte(b':', out, &mut pos)
                && write_byte(partition_type_char(part.part_type), out, &mut pos)
                && write_bytes(b":0x", out, &mut pos)
                && write_u32_hex(part.offset, out, &mut pos)
                && write_bytes(b":0x", out, &mut pos)
                && write_u32_hex(part.size, out, &mut pos)
        })
        && write_bytes(b"\r\n", out, &mut pos);
    ok.then_some(pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Task, MAX_LINE, MAX_NAME_LEN, MAX_PARTS_LINE, MAX_TASKS};

    fn make_startup(reason: ResetReason) -> Startup {
        let mut chip = heapless::String::new();
        let _ = chip.push_str("esp32s3");
        Startup {
            reason,
            chip,
            cores: 2,
            revision: 1,
            mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            flash_size: 4_194_304,
        }
    }

    fn make_frame(
        cpu_cores: u8,
        wifi_rssi: Option<i32>,
        nvs: Option<(u32, u32)>,
    ) -> Frame {
        let mut cpu_usage = heapless::Vec::new();
        let _ = cpu_usage.push(23u8);
        if cpu_cores >= 2 {
            let _ = cpu_usage.push(45u8);
        }
        Frame {
            timestamp_ms: 12345,
            heap_free: 142_336,
            heap_total: 327_680,
            heap_min_free: 98_304,
            heap_frag: 65_536,
            heap_iram: 45_056,
            heap_psram: 0,
            cpu_usage,
            wifi_rssi,
            nvs,
            tasks: heapless::Vec::new(),
        }
    }

    fn str_of(buf: &[u8], n: usize) -> &str {
        core::str::from_utf8(&buf[..n]).unwrap()
    }

    #[test]
    fn format_start_line_structure() {
        let startup = make_startup(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(123, &startup, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("esp_agent: start reason="), "{s}");
        assert!(s.contains("chip="), "{s}");
        assert!(s.contains("cores="), "{s}");
        assert!(s.contains("rev="), "{s}");
    }

    #[test]
    fn format_start_line_ends_with_crlf() {
        let startup = make_startup(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(0, &startup, &mut buf).unwrap();
        assert!(buf[..n].ends_with(b"\r\n"));
    }

    #[test]
    fn format_start_line_no_binary() {
        let startup = make_startup(ResetReason::Panic);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(999, &startup, &mut buf).unwrap();
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
            let startup = make_startup(reason);
            let mut buf = [0u8; MAX_LINE];
            let n = format_start_line(0, &startup, &mut buf).unwrap();
            let s = str_of(&buf, n);
            assert!(s.contains(expected), "expected {expected:?} in {s:?}");
        }
    }

    #[test]
    fn format_start_line_mac() {
        let startup = make_startup(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(0, &startup, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("mac=AA:BB:CC:DD:EE:FF"), "{s}");
    }

    #[test]
    fn format_start_line_flash_size() {
        let startup = make_startup(ResetReason::PowerOn);
        let mut buf = [0u8; MAX_LINE];
        let n = format_start_line(0, &startup, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("flash=0x400000"), "{s}");
    }

    #[test]
    fn format_telemetry_line_structure() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
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
        let s = str_of(&buf, n);
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
        let s = str_of(&buf, n);
        assert!(s.contains("cpu=23 "), "{s}");
        assert!(!s.contains("cpu=23,"), "{s}");
    }

    #[test]
    fn format_telemetry_line_cpu_dual_core() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("cpu=23,45"), "{s}");
    }

    #[test]
    fn format_telemetry_line_wifi_present() {
        let frame = make_frame(2, Some(-65), None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("wifi=-65"), "{s}");
    }

    #[test]
    fn format_telemetry_line_wifi_absent() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(!s.contains("wifi="), "{s}");
    }

    #[test]
    fn format_telemetry_line_nvs_present() {
        let frame = make_frame(2, None, Some((45, 512)));
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("nvs=45/512"), "{s}");
    }

    #[test]
    fn format_telemetry_line_nvs_absent() {
        let frame = make_frame(2, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(!s.contains("nvs="), "{s}");
    }

    #[test]
    fn format_telemetry_line_tasks_roundtrip() {
        let mut tasks: heapless::Vec<Task, MAX_TASKS> = heapless::Vec::new();
        let mut n1 = heapless::String::new();
        let _ = n1.push_str("main");
        tasks
            .push(Task {
                name: n1,
                state: TaskState::Running,
                hwm: 3200,
                priority: 1,
            })
            .ok();
        let mut n2 = heapless::String::new();
        let _ = n2.push_str("wifi_task");
        tasks
            .push(Task {
                name: n2,
                state: TaskState::Blocked,
                hwm: 1856,
                priority: 5,
            })
            .ok();
        let frame = Frame {
            tasks,
            ..make_frame(2, None, None)
        };
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("main:R:3200:1"), "{s}");
        assert!(s.contains("wifi_task:B:1856:5"), "{s}");
    }

    #[test]
    fn format_telemetry_line_empty_tasks() {
        let frame = make_frame(1, None, None);
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.ends_with("tasks=\r\n"), "{s}");
    }

    #[test]
    fn format_telemetry_line_max_tasks() {
        let mut tasks: heapless::Vec<Task, MAX_TASKS> = heapless::Vec::new();
        for i in 0..MAX_TASKS {
            let mut name: heapless::String<MAX_NAME_LEN> = heapless::String::new();
            let _ = name.push(char::from(b'a' + u8::try_from(i % 26).unwrap()));
            tasks
                .push(Task {
                    name,
                    state: TaskState::Ready,
                    hwm: 1024,
                    priority: 2,
                })
                .ok();
        }
        let frame = Frame {
            tasks,
            ..make_frame(2, None, None)
        };
        let mut buf = [0u8; MAX_LINE];
        let n = format_telemetry_line(&frame, &mut buf).unwrap();
        assert!(
            n < MAX_LINE,
            "32 tasks exceeded MAX_LINE: {n} >= {MAX_LINE}"
        );
        let s = str_of(&buf, n);
        assert!(s.contains("a:r:1024:2"), "{s}");
        assert!(s.contains("b:r:1024:2"), "{s}");
        assert!(s.contains("z:r:1024:2"), "{s}");
    }

    #[test]
    fn format_start_line_buffer_too_small() {
        let startup = make_startup(ResetReason::PowerOn);
        let mut buf = [0u8; 4];
        assert_eq!(format_start_line(0, &startup, &mut buf), None);
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
        let s = str_of(&buf, n);
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
        let mut label = heapless::String::new();
        let _ = label.push_str("nvs");
        let part = Partition {
            label,
            part_type: PartType::Data,
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
        let mut label = heapless::String::new();
        let _ = label.push_str("ota_0");
        let part = Partition {
            label,
            part_type: PartType::App,
            offset: 0x10000,
            size: 1_572_864,
        };
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[part], &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("ota_0:a:0x10000:0x180000"), "{s}");
    }

    #[test]
    fn format_parts_line_app_type() {
        let mut label = heapless::String::new();
        let _ = label.push_str("app0");
        let part = Partition {
            label,
            part_type: PartType::App,
            offset: 0x10000,
            size: 1024,
        };
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[part], &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("app0:a:"), "{s}");
    }

    #[test]
    fn format_parts_line_data_type() {
        let mut label = heapless::String::new();
        let _ = label.push_str("nvs");
        let part = Partition {
            label,
            part_type: PartType::Data,
            offset: 0x9000,
            size: 24576,
        };
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[part], &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.contains("nvs:d:"), "{s}");
    }

    #[test]
    fn format_parts_line_empty() {
        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = format_parts_line(0, &[], &mut buf).unwrap();
        let s = str_of(&buf, n);
        assert!(s.ends_with("parts\r\n"), "{s}");
    }

    #[test]
    fn format_parts_line_max_partitions() {
        use crate::{Partition, MAX_PARTITIONS};
        let parts: heapless::Vec<Partition, MAX_PARTITIONS> = (0..MAX_PARTITIONS)
            .map(|i| {
                let mut label: heapless::String<MAX_NAME_LEN> =
                    heapless::String::new();
                let _ = label.push('p');
                let _ = label.push(char::from(b'0' + u8::try_from(i).unwrap()));
                Partition {
                    label,
                    part_type: if i % 2 == 0 {
                        PartType::App
                    } else {
                        PartType::Data
                    },
                    offset: u32::try_from(i).unwrap() * 0x1_0000,
                    size: 65536,
                }
            })
            .collect();
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
    fn write_i32_positive() {
        let mut buf = [0u8; 8];
        let mut pos = 0;
        assert!(write_i32(23, &mut buf, &mut pos));
        assert_eq!(&buf[..pos], b"23");
    }

    #[test]
    fn write_i32_negative() {
        let mut buf = [0u8; 8];
        let mut pos = 0;
        assert!(write_i32(-65, &mut buf, &mut pos));
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
        assert_eq!(partition_type_char(PartType::App), b'a');
        assert_eq!(partition_type_char(PartType::Data), b'd');
        assert_eq!(partition_type_char(PartType::Unknown), b'?');
    }
}

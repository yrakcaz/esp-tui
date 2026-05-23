use crate::{
    Frame, Message, PartType, Partition, ResetReason, Startup, Task, TaskState,
    MAX_NAME_LEN, MAX_PARTITIONS, MAX_STR_LEN, MAX_TASKS,
};

/// Parses an `esp_agent` message string into a [`Message`].
///
/// The input is the message content after the ESP-IDF log prefix has been
/// stripped (i.e. the portion after `esp_agent: `). Returns `None` if the
/// message is malformed or missing required fields.
///
/// # Arguments
///
/// * `message` - Message content string.
///
/// # Returns
///
/// The parsed [`Message`], or `None` if the input could not be parsed.
#[must_use]
pub fn parse(message: &str) -> Option<Message> {
    if message.starts_with("start ") {
        parse_startup(message).map(Message::Startup)
    } else if message == "parts" || message.starts_with("parts ") {
        parse_partitions(message).map(Message::Partitions)
    } else {
        parse_frame(message).map(Message::Frame)
    }
}

fn parse_frame(msg: &str) -> Option<Frame> {
    let (metrics_part, tasks_str) = msg.split_once(" tasks=")?;

    let mut heap_free = None;
    let mut heap_total = None;
    let mut heap_min_free = None;
    let mut heap_frag = None;
    let mut heap_iram = None;
    let mut heap_psram = None;
    let mut cpu_usage: heapless::Vec<u8, 2> = heapless::Vec::new();
    let mut wifi_rssi = None;
    let mut nvs = None;

    for token in metrics_part.split_ascii_whitespace() {
        if let Some(v) = token.strip_prefix("heap=") {
            if let Some((f, t)) = v.split_once('/') {
                if let (Ok(free), Ok(total)) = (f.parse::<u32>(), t.parse::<u32>()) {
                    heap_free = Some(free);
                    heap_total = Some(total);
                }
            }
        } else if let Some(v) = token.strip_prefix("min=") {
            if let Ok(n) = v.parse::<u32>() {
                heap_min_free = Some(n);
            }
        } else if let Some(v) = token.strip_prefix("frag=") {
            if let Ok(n) = v.parse::<u32>() {
                heap_frag = Some(n);
            }
        } else if let Some(v) = token.strip_prefix("iram=") {
            if let Ok(n) = v.parse::<u32>() {
                heap_iram = Some(n);
            }
        } else if let Some(v) = token.strip_prefix("psram=") {
            if let Ok(n) = v.parse::<u32>() {
                heap_psram = Some(n);
            }
        } else if let Some(v) = token.strip_prefix("cpu=") {
            for core_str in v.split(',').take(2) {
                if let Ok(pct) = core_str.parse::<u8>() {
                    cpu_usage.push(pct).ok();
                }
            }
        } else if let Some(v) = token.strip_prefix("wifi=") {
            if let Ok(n) = v.parse::<i32>() {
                wifi_rssi = Some(n);
            }
        } else if let Some(v) = token.strip_prefix("nvs=") {
            if let Some((u, t)) = v.split_once('/') {
                if let (Ok(used), Ok(total)) = (u.parse::<u32>(), t.parse::<u32>()) {
                    nvs = Some((used, total));
                }
            }
        }
    }

    let mut tasks: heapless::Vec<Task, MAX_TASKS> = heapless::Vec::new();
    if !tasks_str.is_empty() {
        for task_str in tasks_str.split(',') {
            if let Some(task) = parse_task(task_str) {
                tasks.push(task).ok();
            }
        }
    }

    Some(Frame {
        timestamp_ms: 0,
        heap_free: heap_free?,
        heap_total: heap_total?,
        heap_min_free: heap_min_free?,
        heap_frag: heap_frag?,
        heap_iram: heap_iram?,
        heap_psram: heap_psram?,
        cpu_usage,
        wifi_rssi,
        nvs,
        tasks,
    })
}

fn parse_task(s: &str) -> Option<Task> {
    let mut it = s.splitn(4, ':');
    let name_str = it.next()?;
    let state_str = it.next()?;
    let hwm_str = it.next()?;
    let prio_str = it.next()?;

    let state = match state_str {
        "R" => TaskState::Running,
        "r" => TaskState::Ready,
        "B" => TaskState::Blocked,
        "S" => TaskState::Suspended,
        "D" => TaskState::Deleted,
        _ => return None,
    };

    let mut name: heapless::String<MAX_NAME_LEN> = heapless::String::new();
    let _ = name.push_str(name_str);

    Some(Task {
        name,
        state,
        hwm: hwm_str.parse().ok()?,
        priority: prio_str.parse().ok()?,
    })
}

fn parse_startup(msg: &str) -> Option<Startup> {
    let rest = msg.strip_prefix("start ")?;

    let mut reason = None;
    let mut chip: Option<heapless::String<MAX_STR_LEN>> = None;
    let mut cores = None;
    let mut revision = None;
    let mut mac = None;
    let mut flash_size = None;

    for token in rest.split_ascii_whitespace() {
        if let Some(v) = token.strip_prefix("reason=") {
            reason = Some(parse_reset_reason(v));
        } else if let Some(v) = token.strip_prefix("chip=") {
            let mut s: heapless::String<MAX_STR_LEN> = heapless::String::new();
            let _ = s.push_str(v);
            chip = Some(s);
        } else if let Some(v) = token.strip_prefix("cores=") {
            if let Ok(n) = v.parse::<u8>() {
                cores = Some(n);
            }
        } else if let Some(v) = token.strip_prefix("rev=") {
            if let Ok(n) = v.parse::<u16>() {
                revision = Some(n);
            }
        } else if let Some(v) = token.strip_prefix("mac=") {
            mac = parse_mac(v);
        } else if let Some(v) = token.strip_prefix("flash=") {
            let hex = v.strip_prefix("0x").unwrap_or(v);
            if let Ok(n) = u32::from_str_radix(hex, 16) {
                flash_size = Some(n);
            }
        }
    }

    Some(Startup {
        reason: reason?,
        chip: chip?,
        cores: cores?,
        revision: revision?,
        mac: mac?,
        flash_size: flash_size?,
    })
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut count = 0usize;
    for part in s.split(':') {
        if count >= 6 {
            return None;
        }
        mac[count] = u8::from_str_radix(part, 16).ok()?;
        count += 1;
    }
    (count == 6).then_some(mac)
}

fn parse_reset_reason(s: &str) -> ResetReason {
    match s {
        "poweron" => ResetReason::PowerOn,
        "sw" => ResetReason::Software,
        "panic" => ResetReason::Panic,
        "int_wdt" => ResetReason::IntWatchdog,
        "task_wdt" => ResetReason::TaskWatchdog,
        "wdt" => ResetReason::Watchdog,
        "brownout" => ResetReason::Brownout,
        "deepsleep" => ResetReason::DeepSleep,
        "ext" => ResetReason::External,
        _ => ResetReason::Unknown,
    }
}

fn parse_partitions(msg: &str) -> Option<heapless::Vec<Partition, MAX_PARTITIONS>> {
    let content = msg.strip_prefix("parts")?;
    let entries_str = content.trim_start_matches(' ');
    let mut parts: heapless::Vec<Partition, MAX_PARTITIONS> = heapless::Vec::new();
    if !entries_str.is_empty() {
        for entry_str in entries_str.split(',') {
            if let Some(part) = parse_partition_entry(entry_str) {
                parts.push(part).ok();
            }
        }
    }
    Some(parts)
}

fn parse_partition_entry(s: &str) -> Option<Partition> {
    let mut it = s.splitn(4, ':');
    let label_str = it.next()?;
    let type_str = it.next()?;
    let offset_hex = it.next()?.strip_prefix("0x").unwrap_or("");
    let size_hex = it.next()?.strip_prefix("0x").unwrap_or("");

    let part_type = match type_str {
        "a" => PartType::App,
        "d" => PartType::Data,
        _ => PartType::Unknown,
    };

    let mut label: heapless::String<MAX_NAME_LEN> = heapless::String::new();
    let _ = label.push_str(label_str);

    Some(Partition {
        label,
        part_type,
        offset: u32::from_str_radix(offset_hex, 16).ok()?,
        size: u32::from_str_radix(size_hex, 16).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{format as fmt, Message, MAX_LINE, MAX_PARTS_LINE};

    fn strip_prefix(line: &str) -> &str {
        line.splitn(2, ": ")
            .nth(1)
            .unwrap_or("")
            .trim_end_matches("\r\n")
    }

    fn format_and_parse_frame(frame: &Frame) -> Frame {
        let mut buf = [0u8; MAX_LINE];
        let n = fmt::format_telemetry_line(frame, &mut buf).unwrap();
        let line = core::str::from_utf8(&buf[..n]).unwrap();
        let msg = strip_prefix(line);
        match parse(msg).unwrap() {
            Message::Frame(f) => f,
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    fn format_and_parse_startup(startup: &Startup) -> Startup {
        let mut buf = [0u8; MAX_LINE];
        let n = fmt::format_start_line(0, startup, &mut buf).unwrap();
        let line = core::str::from_utf8(&buf[..n]).unwrap();
        let msg = strip_prefix(line);
        match parse(msg).unwrap() {
            Message::Startup(s) => s,
            other => panic!("expected Startup, got {other:?}"),
        }
    }

    fn make_frame(
        cpu_cores: u8,
        wifi: Option<i32>,
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
            wifi_rssi: wifi,
            nvs,
            tasks: heapless::Vec::new(),
        }
    }

    fn make_startup() -> Startup {
        let mut chip = heapless::String::new();
        let _ = chip.push_str("esp32s3");
        Startup {
            reason: ResetReason::PowerOn,
            chip,
            cores: 2,
            revision: 1,
            mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            flash_size: 4_194_304,
        }
    }

    #[test]
    fn roundtrip_frame_minimal() {
        let frame = make_frame(1, None, None);
        let parsed = format_and_parse_frame(&frame);
        assert_eq!(parsed.heap_free, frame.heap_free);
        assert_eq!(parsed.heap_total, frame.heap_total);
        assert_eq!(parsed.heap_min_free, frame.heap_min_free);
        assert_eq!(parsed.heap_frag, frame.heap_frag);
        assert_eq!(parsed.heap_iram, frame.heap_iram);
        assert_eq!(parsed.heap_psram, frame.heap_psram);
        assert_eq!(parsed.cpu_usage, frame.cpu_usage);
        assert_eq!(parsed.wifi_rssi, None);
        assert_eq!(parsed.nvs, None);
        assert!(parsed.tasks.is_empty());
    }

    #[test]
    fn roundtrip_frame_dual_core() {
        let frame = make_frame(2, None, None);
        let parsed = format_and_parse_frame(&frame);
        assert_eq!(parsed.cpu_usage.len(), 2);
        assert_eq!(parsed.cpu_usage[0], 23);
        assert_eq!(parsed.cpu_usage[1], 45);
    }

    #[test]
    fn roundtrip_frame_with_wifi() {
        let frame = make_frame(2, Some(-65), None);
        let parsed = format_and_parse_frame(&frame);
        assert_eq!(parsed.wifi_rssi, Some(-65));
    }

    #[test]
    fn roundtrip_frame_with_nvs() {
        let frame = make_frame(2, None, Some((45, 512)));
        let parsed = format_and_parse_frame(&frame);
        assert_eq!(parsed.nvs, Some((45, 512)));
    }

    #[test]
    fn roundtrip_frame_with_tasks() {
        let mut tasks: heapless::Vec<Task, MAX_TASKS> = heapless::Vec::new();
        let mut n1: heapless::String<MAX_NAME_LEN> = heapless::String::new();
        let _ = n1.push_str("main");
        tasks
            .push(Task {
                name: n1,
                state: TaskState::Running,
                hwm: 3200,
                priority: 1,
            })
            .ok();
        let mut n2: heapless::String<MAX_NAME_LEN> = heapless::String::new();
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
        let parsed = format_and_parse_frame(&frame);
        assert_eq!(parsed.tasks.len(), 2);
        assert_eq!(parsed.tasks[0].name.as_str(), "main");
        assert_eq!(parsed.tasks[0].state, TaskState::Running);
        assert_eq!(parsed.tasks[0].hwm, 3200);
        assert_eq!(parsed.tasks[1].name.as_str(), "wifi_task");
        assert_eq!(parsed.tasks[1].state, TaskState::Blocked);
    }

    #[test]
    fn roundtrip_startup() {
        let startup = make_startup();
        let parsed = format_and_parse_startup(&startup);
        assert_eq!(parsed.chip.as_str(), "esp32s3");
        assert_eq!(parsed.reason, ResetReason::PowerOn);
        assert_eq!(parsed.cores, 2);
        assert_eq!(parsed.revision, 1);
        assert_eq!(parsed.mac, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(parsed.flash_size, 4_194_304);
    }

    #[test]
    fn roundtrip_startup_all_reasons() {
        let reasons = [
            ResetReason::PowerOn,
            ResetReason::Software,
            ResetReason::Panic,
            ResetReason::IntWatchdog,
            ResetReason::TaskWatchdog,
            ResetReason::Watchdog,
            ResetReason::Brownout,
            ResetReason::DeepSleep,
            ResetReason::External,
        ];
        for reason in reasons {
            let mut chip = heapless::String::new();
            let _ = chip.push_str("esp32");
            let startup = Startup {
                reason,
                chip,
                cores: 1,
                revision: 0,
                mac: [0; 6],
                flash_size: 0,
            };
            let parsed = format_and_parse_startup(&startup);
            assert_eq!(
                parsed.reason, reason,
                "reason {reason:?} did not round-trip"
            );
        }
    }

    #[test]
    fn roundtrip_partitions() {
        let mut parts: heapless::Vec<Partition, MAX_PARTITIONS> =
            heapless::Vec::new();
        let mut l1: heapless::String<MAX_NAME_LEN> = heapless::String::new();
        let _ = l1.push_str("ota_0");
        parts
            .push(Partition {
                label: l1,
                part_type: PartType::App,
                offset: 0x10000,
                size: 0x180000,
            })
            .ok();
        let mut l2: heapless::String<MAX_NAME_LEN> = heapless::String::new();
        let _ = l2.push_str("nvs");
        parts
            .push(Partition {
                label: l2,
                part_type: PartType::Data,
                offset: 0x9000,
                size: 0x6000,
            })
            .ok();

        let mut buf = [0u8; MAX_PARTS_LINE];
        let n = fmt::format_parts_line(0, &parts, &mut buf).unwrap();
        let line = core::str::from_utf8(&buf[..n]).unwrap();
        let msg = strip_prefix(line);
        let parsed = match parse(msg).unwrap() {
            Message::Partitions(p) => p,
            other => panic!("expected Partitions, got {other:?}"),
        };

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].label.as_str(), "ota_0");
        assert_eq!(parsed[0].part_type, PartType::App);
        assert_eq!(parsed[0].offset, 0x10000);
        assert_eq!(parsed[0].size, 0x180000);
        assert_eq!(parsed[1].label.as_str(), "nvs");
        assert_eq!(parsed[1].part_type, PartType::Data);
    }

    #[test]
    fn parse_empty_parts_line() {
        match parse("parts").unwrap() {
            Message::Partitions(p) => assert!(p.is_empty()),
            other => panic!("expected Partitions, got {other:?}"),
        }
    }

    #[test]
    fn parse_malformed_returns_none() {
        assert!(parse(
            "heap=notanumber/327680 min=0 frag=0 iram=0 psram=0 cpu=0 tasks="
        )
        .is_none());
        assert!(parse("start ").is_none());
    }

    #[test]
    fn parse_frame_optional_fields_absent() {
        let frame =
            parse("heap=100/200 min=50 frag=30 iram=10 psram=0 cpu=10 tasks=");
        let f = match frame.unwrap() {
            Message::Frame(f) => f,
            other => panic!("{other:?}"),
        };
        assert_eq!(f.wifi_rssi, None);
        assert_eq!(f.nvs, None);
        assert!(f.tasks.is_empty());
    }

    #[test]
    fn parse_task_all_states() {
        let cases = [
            (
                "heap=1/2 min=1 frag=1 iram=1 psram=0 cpu=0 tasks=t:R:100:1",
                TaskState::Running,
            ),
            (
                "heap=1/2 min=1 frag=1 iram=1 psram=0 cpu=0 tasks=t:r:100:1",
                TaskState::Ready,
            ),
            (
                "heap=1/2 min=1 frag=1 iram=1 psram=0 cpu=0 tasks=t:B:100:1",
                TaskState::Blocked,
            ),
            (
                "heap=1/2 min=1 frag=1 iram=1 psram=0 cpu=0 tasks=t:S:100:1",
                TaskState::Suspended,
            ),
            (
                "heap=1/2 min=1 frag=1 iram=1 psram=0 cpu=0 tasks=t:D:100:1",
                TaskState::Deleted,
            ),
        ];
        for (s, expected) in cases {
            let f = match parse(s).unwrap() {
                Message::Frame(f) => f,
                other => panic!("{other:?}"),
            };
            assert_eq!(f.tasks[0].state, expected);
        }
    }
}

use core::sync::atomic::Ordering;

const STARTUP_REBROADCAST_US: i64 = 10_000_000;

use esp_agent_msg as msg;

use crate::ffi::{
    self, EspChipInfo, EspPartition, NvsStats, TaskStatus, WifiApRecord,
    ESP_PARTITION_SUBTYPE_ANY, ESP_PARTITION_TYPE_ANY, MALLOC_CAP_DEFAULT,
    MALLOC_CAP_INTERNAL, MALLOC_CAP_SPIRAM, RST_BROWNOUT, RST_DEEPSLEEP, RST_EXT,
    RST_INT_WDT, RST_PANIC, RST_POWERON, RST_SW, RST_TASK_WDT, RST_WDT,
    TSK_NO_AFFINITY,
};

#[used]
#[cfg_attr(target_arch = "xtensa", link_section = ".ctors")]
#[cfg_attr(target_arch = "riscv32", link_section = ".init_array")]
static _ESP_AGENT_CTOR: extern "C" fn() = _esp_agent_ctor;

extern "C" fn _esp_agent_ctor() {
    // SAFETY: all arguments are valid: function pointer, null-terminated name,
    // stack size, null param, priority, null handle, tskNO_AFFINITY (-1).
    unsafe {
        ffi::xTaskCreatePinnedToCore(
            agent_task_fn,
            c"esp_agent".as_ptr(),
            8192,
            core::ptr::null_mut(),
            1,
            core::ptr::null_mut(),
            TSK_NO_AFFINITY,
        );
    }
}

/// C ABI entry point for `esp_agent_configure`; delegates to [`crate::configure`].
///
/// # Arguments
///
/// * `interval_ms` - Sampling interval in milliseconds.
#[no_mangle]
pub extern "C" fn esp_agent_configure(interval_ms: u32) {
    crate::configure(interval_ms);
}

fn reset_reason_from_esp(raw: u32) -> msg::ResetReason {
    match raw {
        RST_POWERON => msg::ResetReason::PowerOn,
        RST_SW => msg::ResetReason::Software,
        RST_PANIC => msg::ResetReason::Panic,
        RST_INT_WDT => msg::ResetReason::IntWatchdog,
        RST_TASK_WDT => msg::ResetReason::TaskWatchdog,
        RST_WDT => msg::ResetReason::Watchdog,
        RST_BROWNOUT => msg::ResetReason::Brownout,
        RST_DEEPSLEEP => msg::ResetReason::DeepSleep,
        RST_EXT => msg::ResetReason::External,
        _ => msg::ResetReason::Unknown,
    }
}

/// Copies a null-terminated C string into `dst`, stopping at the first null
/// byte or when `dst` is full.
///
/// # Arguments
///
/// * `src` - Pointer to a null-terminated C string.
/// * `dst` - Destination byte slice.
///
/// # Returns
///
/// Number of bytes copied (excluding the null terminator).
///
/// # Safety
///
/// `src` must point to a valid null-terminated string that remains valid for
/// the duration of this call.
unsafe fn copy_cstr(src: *const core::ffi::c_char, dst: &mut [u8]) -> usize {
    let mut len = 0;
    while len < dst.len() {
        let b = *src.add(len).cast::<u8>();
        if b == 0 {
            break;
        }
        dst[len] = b;
        len += 1;
    }
    len
}

fn build_startup_info() -> msg::Startup {
    let mut chip_info = EspChipInfo {
        model: 0,
        features: 0,
        revision: 0,
        cores: 0,
        _pad: 0,
    };
    // SAFETY: chip_info is a valid local; esp_chip_info fills all fields.
    unsafe { ffi::esp_chip_info(&raw mut chip_info) };

    let model_bytes = ffi::chip_name_for_model(chip_info.model);
    let mut chip: heapless::String<{ msg::MAX_STR_LEN }> = heapless::String::new();
    if let Ok(s) = core::str::from_utf8(model_bytes) {
        let _ = chip.push_str(s);
    }

    let mut mac = [0u8; 6];
    // SAFETY: mac is exactly 6 bytes; type 0 = ESP_MAC_WIFI_STA.
    unsafe { ffi::esp_read_mac(mac.as_mut_ptr(), 0) };

    let mut flash_size = 0u32;
    // SAFETY: NULL chip selects the default flash; out_size is a valid pointer.
    unsafe { ffi::esp_flash_get_size(core::ptr::null_mut(), &raw mut flash_size) };

    // SAFETY: pure query; no arguments.
    let raw_reason = unsafe { ffi::esp_reset_reason() };

    msg::Startup {
        reason: reset_reason_from_esp(raw_reason),
        chip,
        cores: chip_info.cores,
        revision: chip_info.revision,
        mac,
        flash_size,
    }
}

fn collect_partitions() -> heapless::Vec<msg::Partition, { msg::MAX_PARTITIONS }> {
    let mut parts: heapless::Vec<msg::Partition, { msg::MAX_PARTITIONS }> =
        heapless::Vec::new();

    // SAFETY: ANY/ANY with a null label enumerates all partitions.
    let mut iter = unsafe {
        ffi::esp_partition_find(
            ESP_PARTITION_TYPE_ANY,
            ESP_PARTITION_SUBTYPE_ANY,
            core::ptr::null(),
        )
    };

    while !iter.is_null() {
        // SAFETY: iter is non-null; esp_partition_get returns a pointer to a
        // static partition descriptor owned by the IDF partition table.
        let p: *const EspPartition = unsafe { ffi::esp_partition_get(iter) };
        if !p.is_null() {
            // SAFETY: p is non-null and points to a valid esp_partition_t.
            let part = unsafe { &*p };
            let label_raw = &part.label;
            let label_len = label_raw
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(label_raw.len());
            let mut label: heapless::String<{ msg::MAX_NAME_LEN }> =
                heapless::String::new();
            if let Ok(s) = core::str::from_utf8(&label_raw[..label_len]) {
                let _ = label.push_str(s);
            }
            let part_type = match u8::try_from(part.type_).unwrap_or(u8::MAX) {
                0 => msg::PartType::App,
                1 => msg::PartType::Data,
                _ => msg::PartType::Unknown,
            };
            if parts
                .push(msg::Partition {
                    label,
                    part_type,
                    offset: part.address,
                    size: part.size,
                })
                .is_err()
            {
                break;
            }
        }
        // SAFETY: iter is non-null.
        iter = unsafe { ffi::esp_partition_next(iter) };
    }

    // SAFETY: esp_partition_iterator_release handles NULL gracefully.
    unsafe { ffi::esp_partition_iterator_release(iter) };

    parts
}

fn build_task_info(status: &TaskStatus) -> msg::Task {
    let mut name: heapless::String<{ msg::MAX_NAME_LEN }> = heapless::String::new();
    let mut name_buf = [0u8; msg::MAX_NAME_LEN];
    // SAFETY: status.name is a valid null-terminated C string; FreeRTOS task
    // descriptors are static and outlive this call.
    let name_len = unsafe { copy_cstr(status.name, &mut name_buf) };
    if let Ok(s) = core::str::from_utf8(&name_buf[..name_len]) {
        let _ = name.push_str(s);
    }
    msg::Task {
        name,
        state: msg::TaskState::from_u32(status.current_state),
        hwm: status.stack_high_water_mark,
        priority: status.current_priority,
    }
}

fn idle_runtime(
    statuses: &[TaskStatus; msg::MAX_TASKS],
    filled: usize,
    core_idx: usize,
) -> u32 {
    let target: &[u8] = if core_idx == 0 { b"IDLE0" } else { b"IDLE1" };
    statuses[..filled]
        .iter()
        .find_map(|s| {
            if s.name.is_null() {
                None
            } else {
                let mut name = [0u8; 8];
                // SAFETY: s.name is a valid null-terminated FreeRTOS task name.
                let len = unsafe { copy_cstr(s.name, &mut name) };
                (name[..len] == *target).then_some(s.runtime_counter)
            }
        })
        .unwrap_or(0)
}

fn cpu_percent(idle_delta: u32, total_delta: u32) -> u8 {
    if total_delta == 0 {
        0
    } else {
        let idle_pct = u64::from(idle_delta) * 100 / u64::from(total_delta);
        100u8.saturating_sub(u8::try_from(idle_pct.min(100)).unwrap_or(100))
    }
}

fn console_write(buf: &[u8]) {
    // SAFETY: buf is valid for buf.len() bytes; fd 1 is always open.
    unsafe { ffi::write(1, buf.as_ptr().cast(), buf.len()) };
}

fn emit_startup_lines(startup: &msg::Startup) {
    let mut start_buf = [0u8; msg::MAX_LINE];
    if let Some(n) = msg::format::format_start_line(
        // SAFETY: pure query.
        unsafe { ffi::xTaskGetTickCount() },
        startup,
        &mut start_buf,
    ) {
        console_write(&start_buf[..n]);
    }

    let parts = collect_partitions();
    let mut parts_buf = [0u8; msg::MAX_PARTS_LINE];
    if let Some(n) = msg::format::format_parts_line(
        // SAFETY: pure query.
        unsafe { ffi::xTaskGetTickCount() },
        &parts,
        &mut parts_buf,
    ) {
        console_write(&parts_buf[..n]);
    }
}

fn run_iteration(
    cores: usize,
    prev_total_runtime: &mut u32,
    prev_idle: &mut [u32; 2],
) {
    let heap_free =
        u32::try_from(unsafe { ffi::heap_caps_get_free_size(MALLOC_CAP_DEFAULT) })
            .unwrap_or(u32::MAX);
    let heap_total =
        u32::try_from(unsafe { ffi::heap_caps_get_total_size(MALLOC_CAP_DEFAULT) })
            .unwrap_or(u32::MAX);
    let heap_min_free = u32::try_from(unsafe {
        ffi::heap_caps_get_minimum_free_size(MALLOC_CAP_DEFAULT)
    })
    .unwrap_or(u32::MAX);
    let heap_frag = u32::try_from(unsafe {
        ffi::heap_caps_get_largest_free_block(MALLOC_CAP_DEFAULT)
    })
    .unwrap_or(u32::MAX);
    let heap_iram =
        u32::try_from(unsafe { ffi::heap_caps_get_free_size(MALLOC_CAP_INTERNAL) })
            .unwrap_or(u32::MAX);
    let heap_psram =
        u32::try_from(unsafe { ffi::heap_caps_get_free_size(MALLOC_CAP_SPIRAM) })
            .unwrap_or(0);

    let mut curr_statuses: [TaskStatus; msg::MAX_TASKS] =
        unsafe { core::mem::zeroed() };
    let mut curr_total_runtime = 0u32;
    let max_tasks = u32::try_from(msg::MAX_TASKS).unwrap_or(32);
    let curr_filled = unsafe {
        ffi::uxTaskGetSystemState(
            curr_statuses.as_mut_ptr(),
            max_tasks,
            &raw mut curr_total_runtime,
        )
    } as usize;

    let total_delta = curr_total_runtime.wrapping_sub(*prev_total_runtime);
    let mut cpu_usage: heapless::Vec<u8, 2> = heapless::Vec::new();
    for c in 0..cores {
        let curr_idle = idle_runtime(&curr_statuses, curr_filled, c);
        let idle_delta = curr_idle.wrapping_sub(prev_idle[c]);
        let _ = cpu_usage.push(cpu_percent(idle_delta, total_delta));
        prev_idle[c] = curr_idle;
    }
    *prev_total_runtime = curr_total_runtime;

    let (wifi_rssi, wifi_channel) = {
        let mut ap: WifiApRecord = unsafe { core::mem::zeroed() };
        // SAFETY: ap is properly sized; returns non-zero if WiFi not connected.
        let ret = unsafe { ffi::esp_wifi_sta_get_ap_info(&raw mut ap) };
        if ret == 0 {
            (Some(i32::from(ap.rssi)), Some(ap.primary))
        } else {
            (None, None)
        }
    };

    let nvs = {
        let mut stats = NvsStats {
            used_entries: 0,
            free_entries: 0,
            total_entries: 0,
            namespace_count: 0,
        };
        // SAFETY: null selects the default NVS partition; stats is valid.
        let ret = unsafe { ffi::nvs_get_stats(core::ptr::null(), &raw mut stats) };
        (ret == 0).then_some((
            u32::try_from(stats.used_entries).unwrap_or(u32::MAX),
            u32::try_from(stats.total_entries).unwrap_or(u32::MAX),
        ))
    };

    let mut tasks: heapless::Vec<msg::Task, { msg::MAX_TASKS }> =
        heapless::Vec::new();
    for status in &curr_statuses[..curr_filled.min(msg::MAX_TASKS)] {
        tasks.push(build_task_info(status)).ok();
    }

    let frame = msg::Frame {
        // SAFETY: pure query.
        timestamp_ms: unsafe { ffi::xTaskGetTickCount() },
        heap_free,
        heap_total,
        heap_min_free,
        heap_frag,
        heap_iram,
        heap_psram,
        cpu_usage,
        wifi_rssi,
        wifi_channel,
        nvs,
        tasks,
    };

    let mut line_buf = [0u8; msg::MAX_LINE];
    if let Some(n) = msg::format::format_telemetry_line(&frame, &mut line_buf) {
        console_write(&line_buf[..n]);
    }
}

unsafe extern "C" fn agent_task_fn(_param: *mut core::ffi::c_void) {
    let startup = build_startup_info();
    emit_startup_lines(&startup);

    let cores = usize::from(startup.cores.clamp(1, 2));
    let mut prev_total_runtime = 0u32;
    let mut prev_idle: [u32; 2] = [0; 2];
    // SAFETY: esp_timer_get_time returns monotonic microseconds since boot.
    let mut last_startup_emit_us = unsafe { ffi::esp_timer_get_time() };

    loop {
        let interval_ms = crate::AGENT_INTERVAL.load(Ordering::Relaxed);
        if interval_ms == 0 {
            // SAFETY: portMAX_DELAY (0xFFFF_FFFF) suspends indefinitely.
            unsafe { ffi::vTaskDelay(0xFFFF_FFFF) };
            continue;
        }
        // SAFETY: esp_timer_get_time returns monotonic microseconds since boot.
        let now_us = unsafe { ffi::esp_timer_get_time() };
        let deadline_us = now_us + i64::from(interval_ms) * 1000;
        run_iteration(cores, &mut prev_total_runtime, &mut prev_idle);
        if now_us - last_startup_emit_us >= STARTUP_REBROADCAST_US {
            emit_startup_lines(&startup);
            last_startup_emit_us = now_us;
        }
        // Yield in single-tick increments until the interval elapses, avoiding
        // any dependency on configTICK_RATE_HZ.
        // SAFETY: vTaskDelay(1) yields for one tick; esp_timer_get_time is safe.
        while unsafe { ffi::esp_timer_get_time() } < deadline_us {
            unsafe { ffi::vTaskDelay(1) };
        }
    }
}

use core::sync::atomic::Ordering;

use crate::ffi::{
    self, EspChipInfo, EspPartition, NvsStats, TaskStatus, WifiApRecord,
    CONFIG_FREERTOS_HZ, ESP_PARTITION_SUBTYPE_ANY, ESP_PARTITION_TYPE_ANY,
    MALLOC_CAP_DEFAULT, MALLOC_CAP_INTERNAL, MALLOC_CAP_SPIRAM, RST_BROWNOUT,
    RST_DEEPSLEEP, RST_EXT, RST_INT_WDT, RST_PANIC, RST_POWERON, RST_SW,
    RST_TASK_WDT, RST_WDT, TSK_NO_AFFINITY,
};
use crate::fmt::{
    self, PartitionEntry, ResetReason, StartupInfo, TaskInfo, TelemetryFrame,
    EMPTY_TASK_INFO, MAX_LINE, MAX_PARTITIONS, MAX_PARTITION_LABEL, MAX_PARTS_LINE,
    MAX_TASKS, MAX_TASK_NAME,
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

fn reset_reason_from_esp(raw: u32) -> ResetReason {
    match raw {
        RST_POWERON => ResetReason::PowerOn,
        RST_SW => ResetReason::Software,
        RST_PANIC => ResetReason::Panic,
        RST_INT_WDT => ResetReason::IntWatchdog,
        RST_TASK_WDT => ResetReason::TaskWatchdog,
        RST_WDT => ResetReason::Watchdog,
        RST_BROWNOUT => ResetReason::Brownout,
        RST_DEEPSLEEP => ResetReason::DeepSleep,
        RST_EXT => ResetReason::External,
        _ => ResetReason::Unknown,
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

fn build_startup_info() -> StartupInfo {
    let mut chip = EspChipInfo {
        model: 0,
        features: 0,
        revision: 0,
        cores: 0,
        _pad: 0,
    };
    // SAFETY: chip is a valid local; esp_chip_info fills all fields.
    unsafe { ffi::esp_chip_info(&raw mut chip) };

    let model_name = ffi::chip_name_for_model(chip.model);
    let mut chip_name = [0u8; 16];
    let name_len = model_name.len().min(chip_name.len());
    chip_name[..name_len].copy_from_slice(&model_name[..name_len]);

    let mut mac = [0u8; 6];
    // SAFETY: mac is exactly 6 bytes; type 0 = ESP_MAC_WIFI_STA.
    unsafe { ffi::esp_read_mac(mac.as_mut_ptr(), 0) };

    let mut flash_size = 0u32;
    // SAFETY: NULL chip selects the default flash; out_size is a valid pointer.
    unsafe { ffi::esp_flash_get_size(core::ptr::null_mut(), &raw mut flash_size) };

    // SAFETY: pure query; no arguments.
    let raw_reason = unsafe { ffi::esp_reset_reason() };

    StartupInfo {
        reason: reset_reason_from_esp(raw_reason),
        chip_name,
        chip_name_len: name_len,
        cores: chip.cores,
        revision: chip.revision,
        mac,
        flash_size,
    }
}

fn collect_partitions() -> ([PartitionEntry; MAX_PARTITIONS], usize) {
    let mut parts = [PartitionEntry {
        label: [0u8; MAX_PARTITION_LABEL],
        label_len: 0,
        type_: 0,
        offset: 0,
        size: 0,
    }; MAX_PARTITIONS];
    let mut count = 0usize;

    // SAFETY: ANY/ANY with a null label enumerates all partitions.
    let mut iter = unsafe {
        ffi::esp_partition_find(
            ESP_PARTITION_TYPE_ANY,
            ESP_PARTITION_SUBTYPE_ANY,
            core::ptr::null(),
        )
    };

    while !iter.is_null() && count < MAX_PARTITIONS {
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
                .unwrap_or(label_raw.len())
                .min(MAX_PARTITION_LABEL);
            let mut label = [0u8; MAX_PARTITION_LABEL];
            label[..label_len].copy_from_slice(&label_raw[..label_len]);

            parts[count] = PartitionEntry {
                label,
                label_len,
                type_: u8::try_from(part.type_).unwrap_or(u8::MAX),
                offset: part.address,
                size: part.size,
            };
            count += 1;
        }
        // SAFETY: iter is non-null.
        iter = unsafe { ffi::esp_partition_next(iter) };
    }

    // SAFETY: esp_partition_iterator_release handles NULL gracefully.
    unsafe { ffi::esp_partition_iterator_release(iter) };

    (parts, count)
}

fn build_task_info(status: &TaskStatus) -> TaskInfo {
    let mut name = [0u8; MAX_TASK_NAME];
    // SAFETY: status.name is a valid null-terminated C string; FreeRTOS task
    // descriptors are static and outlive this call.
    let name_len = unsafe { copy_cstr(status.name, &mut name) };
    TaskInfo {
        name,
        name_len,
        state: fmt::task_state_from_u32(status.current_state),
        stack_hwm: status.stack_high_water_mark,
        priority: status.current_priority,
    }
}

fn idle_runtime(
    statuses: &[TaskStatus; MAX_TASKS],
    filled: usize,
    core_idx: usize,
) -> u32 {
    let target: &[u8] = if core_idx == 0 { b"IDLE0" } else { b"IDLE1" };
    statuses[..filled]
        .iter()
        .find_map(|s| {
            if s.name.is_null() {
                return None;
            }
            let mut name = [0u8; 8];
            // SAFETY: s.name is a valid null-terminated FreeRTOS task name.
            let len = unsafe { copy_cstr(s.name, &mut name) };
            (name[..len] == *target).then_some(s.runtime_counter)
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

fn emit_startup_lines(startup: &StartupInfo) {
    let mut start_buf = [0u8; MAX_LINE];
    if let Some(n) = fmt::format_start_line(
        // SAFETY: pure query.
        unsafe { ffi::xTaskGetTickCount() },
        startup,
        &mut start_buf,
    ) {
        console_write(&start_buf[..n]);
    }

    let (parts, part_count) = collect_partitions();
    let mut parts_buf = [0u8; MAX_PARTS_LINE];
    if let Some(n) = fmt::format_parts_line(
        // SAFETY: pure query.
        unsafe { ffi::xTaskGetTickCount() },
        &parts[..part_count],
        &mut parts_buf,
    ) {
        console_write(&parts_buf[..n]);
    }
}

fn run_iteration(
    cores_u8: u8,
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

    let mut curr_statuses: [TaskStatus; MAX_TASKS] = unsafe { core::mem::zeroed() };
    let mut curr_total_runtime = 0u32;
    let max_tasks = u32::try_from(MAX_TASKS).unwrap_or(32);
    let curr_filled = unsafe {
        ffi::uxTaskGetSystemState(
            curr_statuses.as_mut_ptr(),
            max_tasks,
            &raw mut curr_total_runtime,
        )
    } as usize;

    let total_delta = curr_total_runtime.wrapping_sub(*prev_total_runtime);
    let mut cpu_usage = [0u8; 2];
    for c in 0..cores {
        let curr_idle = idle_runtime(&curr_statuses, curr_filled, c);
        let idle_delta = curr_idle.wrapping_sub(prev_idle[c]);
        cpu_usage[c] = cpu_percent(idle_delta, total_delta);
        prev_idle[c] = curr_idle;
    }
    *prev_total_runtime = curr_total_runtime;

    let wifi_rssi = {
        let mut ap: WifiApRecord = unsafe { core::mem::zeroed() };
        // SAFETY: ap is properly sized; returns non-zero if WiFi not connected.
        let ret = unsafe { ffi::esp_wifi_sta_get_ap_info(&raw mut ap) };
        (ret == 0).then_some(ap.rssi)
    };

    let nvs_used_total = {
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

    let mut tasks = [EMPTY_TASK_INFO; MAX_TASKS];
    let task_count = curr_filled.min(MAX_TASKS);
    for (i, status) in curr_statuses[..task_count].iter().enumerate() {
        tasks[i] = build_task_info(status);
    }

    let frame = TelemetryFrame {
        // SAFETY: pure query.
        timestamp_ms: unsafe { ffi::xTaskGetTickCount() },
        heap_free,
        heap_total,
        heap_min_free,
        heap_frag,
        heap_iram,
        heap_psram,
        cpu_cores: cores_u8,
        cpu_usage,
        wifi_rssi,
        nvs_used: nvs_used_total.map(|(u, _)| u),
        nvs_total: nvs_used_total.map(|(_, t)| t),
        task_count,
        tasks,
    };

    let mut line_buf = [0u8; MAX_LINE];
    if let Some(n) = fmt::format_telemetry_line(&frame, &mut line_buf) {
        console_write(&line_buf[..n]);
    }
}

unsafe extern "C" fn agent_task_fn(_param: *mut core::ffi::c_void) {
    let startup = build_startup_info();
    emit_startup_lines(&startup);

    let cores_u8 = startup.cores.clamp(1, 2);
    let cores = usize::from(cores_u8);
    let mut prev_total_runtime = 0u32;
    let mut prev_idle: [u32; 2] = [0; 2];

    loop {
        let interval_ms = crate::AGENT_INTERVAL.load(Ordering::Relaxed);
        if interval_ms == 0 {
            // SAFETY: portMAX_DELAY (0xFFFF_FFFF) suspends indefinitely.
            unsafe { ffi::vTaskDelay(0xFFFF_FFFF) };
            continue;
        }
        // SAFETY: ticks is a valid FreeRTOS tick count.
        unsafe { ffi::vTaskDelay(interval_ms * CONFIG_FREERTOS_HZ / 1000) };
        run_iteration(cores_u8, cores, &mut prev_total_runtime, &mut prev_idle);
    }
}

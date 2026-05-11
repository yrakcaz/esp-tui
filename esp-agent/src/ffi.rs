use core::ffi::{c_char, c_int, c_void};

pub(crate) type TaskHandle = *mut c_void;
pub(crate) type UartPortT = c_int;
pub(crate) type PartitionIter = *mut c_void;

/// Matches `TaskStatus_t` from `FreeRTOS` `task.h`.
#[repr(C)]
pub(crate) struct TaskStatus {
    pub(crate) handle: TaskHandle,
    pub(crate) name: *const c_char,
    pub(crate) task_number: u32,
    pub(crate) current_state: u32,
    pub(crate) current_priority: u32,
    pub(crate) base_priority: u32,
    pub(crate) runtime_counter: u32,
    pub(crate) stack_base: *mut u8,
    pub(crate) stack_high_water_mark: u32,
}

/// Matches `esp_partition_t` (fields we need; must stay layout-compatible).
#[repr(C)]
pub(crate) struct EspPartition {
    pub(crate) flash_chip: *mut c_void,
    pub(crate) type_: u32,
    pub(crate) subtype: u32,
    pub(crate) address: u32,
    pub(crate) size: u32,
    pub(crate) erase_size: u32,
    pub(crate) label: [u8; 17],
    pub(crate) encrypted: bool,
    pub(crate) readonly: bool,
}

/// Matches `wifi_ap_record_t` prefix (only `rssi` is read; `_pad` covers the rest).
#[repr(C)]
pub(crate) struct WifiApRecord {
    pub(crate) bssid: [u8; 6],
    pub(crate) ssid: [u8; 33],
    pub(crate) primary: u8,
    pub(crate) second: u32,
    pub(crate) rssi: i8,
    pub(crate) _pad: [u8; 64],
}

/// Matches `nvs_stats_t` from `nvs.h`.
#[repr(C)]
pub(crate) struct NvsStats {
    pub(crate) used_entries: usize,
    pub(crate) free_entries: usize,
    pub(crate) total_entries: usize,
    pub(crate) namespace_count: usize,
}

/// Matches `esp_chip_info_t` from `esp_chip_info.h`.
#[repr(C)]
pub(crate) struct EspChipInfo {
    pub(crate) model: u32,
    pub(crate) features: u32,
    pub(crate) revision: u16,
    pub(crate) cores: u8,
    pub(crate) _pad: u8,
}

pub(crate) const TSK_NO_AFFINITY: i32 = 0x7FFF_FFFF;

pub(crate) const RST_POWERON: u32 = 1;
pub(crate) const RST_EXT: u32 = 2;
pub(crate) const RST_SW: u32 = 3;
pub(crate) const RST_PANIC: u32 = 4;
pub(crate) const RST_INT_WDT: u32 = 5;
pub(crate) const RST_TASK_WDT: u32 = 6;
pub(crate) const RST_WDT: u32 = 7;
pub(crate) const RST_DEEPSLEEP: u32 = 8;
pub(crate) const RST_BROWNOUT: u32 = 9;

pub(crate) const CHIP_ESP32: u32 = 1;
pub(crate) const CHIP_ESP32S2: u32 = 2;
pub(crate) const CHIP_ESP32S3: u32 = 9;
pub(crate) const CHIP_ESP32C3: u32 = 5;
pub(crate) const CHIP_ESP32C2: u32 = 12;
pub(crate) const CHIP_ESP32C6: u32 = 13;
pub(crate) const CHIP_ESP32H2: u32 = 16;

pub(crate) const MALLOC_CAP_DEFAULT: u32 = 1 << 12;
pub(crate) const MALLOC_CAP_INTERNAL: u32 = 1 << 3;
pub(crate) const MALLOC_CAP_SPIRAM: u32 = 1 << 9;
pub(crate) const ESP_PARTITION_TYPE_ANY: u32 = 0xFF;
pub(crate) const ESP_PARTITION_SUBTYPE_ANY: u32 = 0xFF;
pub(crate) const CONFIG_FREERTOS_HZ: u32 = 1000;

extern "C" {
    pub(crate) fn heap_caps_get_free_size(caps: u32) -> usize;
    pub(crate) fn heap_caps_get_minimum_free_size(caps: u32) -> usize;
    pub(crate) fn heap_caps_get_total_size(caps: u32) -> usize;
    pub(crate) fn heap_caps_get_largest_free_block(caps: u32) -> usize;

    pub(crate) fn uxTaskGetSystemState(
        arr: *mut TaskStatus,
        size: u32,
        total: *mut u32,
    ) -> u32;
    pub(crate) fn xTaskCreatePinnedToCore(
        f: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        stack: u32,
        param: *mut c_void,
        prio: u32,
        handle: *mut TaskHandle,
        core: i32,
    ) -> i32;
    pub(crate) fn vTaskDelay(ticks: u32);
    pub(crate) fn xTaskGetTickCount() -> u32;

    pub(crate) fn esp_partition_find(
        type_: u32,
        subtype: u32,
        label: *const c_char,
    ) -> PartitionIter;
    pub(crate) fn esp_partition_next(iter: PartitionIter) -> PartitionIter;
    pub(crate) fn esp_partition_get(iter: PartitionIter) -> *const EspPartition;
    pub(crate) fn esp_partition_iterator_release(iter: PartitionIter);

    pub(crate) fn esp_wifi_sta_get_ap_info(ap_info: *mut WifiApRecord) -> c_int;

    pub(crate) fn nvs_get_stats(
        part_name: *const c_char,
        stats: *mut NvsStats,
    ) -> c_int;

    pub(crate) fn esp_reset_reason() -> u32;
    pub(crate) fn esp_chip_info(info: *mut EspChipInfo);
    pub(crate) fn esp_read_mac(mac: *mut u8, type_: u32) -> c_int;
    pub(crate) fn esp_flash_get_size(chip: *mut c_void, out_size: *mut u32)
        -> c_int;

    pub(crate) fn uart_write_bytes(
        port: UartPortT,
        src: *const c_void,
        size: usize,
    ) -> c_int;
}

/// Returns the chip name bytes for a given `esp_chip_model_t` value.
///
/// # Arguments
///
/// * `model` - The `model` field from `esp_chip_info_t`.
///
/// # Returns
///
/// A static byte slice containing the ASCII chip name, or `b"unknown"`.
pub(crate) fn chip_name_for_model(model: u32) -> &'static [u8] {
    match model {
        CHIP_ESP32 => b"esp32",
        CHIP_ESP32S2 => b"esp32s2",
        CHIP_ESP32S3 => b"esp32s3",
        CHIP_ESP32C3 => b"esp32c3",
        CHIP_ESP32C2 => b"esp32c2",
        CHIP_ESP32C6 => b"esp32c6",
        CHIP_ESP32H2 => b"esp32h2",
        _ => b"unknown",
    }
}

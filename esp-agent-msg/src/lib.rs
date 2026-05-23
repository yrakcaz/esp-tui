#![no_std]

pub mod format;
pub mod parse;

/// Maximum number of `FreeRTOS` tasks captured per telemetry frame.
pub const MAX_TASKS: usize = 32;

/// Maximum number of partition table entries captured per parts line.
pub const MAX_PARTITIONS: usize = 16;

/// Maximum byte length of a task name (matching `FreeRTOS` `configMAX_TASK_NAME_LEN`).
pub const MAX_NAME_LEN: usize = 16;

/// Maximum byte length of a short string field (chip name, reset reason).
pub const MAX_STR_LEN: usize = 32;

/// Maximum byte length of a formatted telemetry or start line.
pub const MAX_LINE: usize = 768;

/// Maximum byte length of a formatted partition line.
pub const MAX_PARTS_LINE: usize = 512;

/// `FreeRTOS` task scheduling state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Ready,
    Blocked,
    Suspended,
    Deleted,
}

impl TaskState {
    /// Converts a `FreeRTOS` task state integer to [`TaskState`].
    ///
    /// # Arguments
    ///
    /// * `state` - The integer value from `TaskStatus.current_state`.
    ///
    /// # Returns
    ///
    /// The matching variant; out-of-range values map to [`TaskState::Deleted`].
    #[must_use]
    pub fn from_u32(state: u32) -> Self {
        match state {
            0 => Self::Running,
            1 => Self::Ready,
            2 => Self::Blocked,
            3 => Self::Suspended,
            _ => Self::Deleted,
        }
    }
}

/// One `FreeRTOS` task entry in a telemetry frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Task name.
    pub name: heapless::String<MAX_NAME_LEN>,
    /// Current scheduling state.
    pub state: TaskState,
    /// Stack high-water mark in bytes (minimum free stack ever observed).
    pub hwm: u32,
    /// Current task priority.
    pub priority: u32,
}

/// A snapshot of device metrics sampled in one agent task iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Tick count at the time of sampling, in milliseconds.
    pub timestamp_ms: u32,
    /// Default-heap free bytes (`MALLOC_CAP_DEFAULT`).
    pub heap_free: u32,
    /// Default-heap total bytes.
    pub heap_total: u32,
    /// Minimum free heap ever observed (low-water mark).
    pub heap_min_free: u32,
    /// Largest contiguous free block in the default heap.
    pub heap_frag: u32,
    /// Internal SRAM free bytes (`MALLOC_CAP_INTERNAL`).
    pub heap_iram: u32,
    /// PSRAM free bytes; `0` if no PSRAM is present or configured.
    pub heap_psram: u32,
    /// Per-core CPU usage as integer percentages; length is 1 for single-core, 2 for dual.
    pub cpu_usage: heapless::Vec<u8, 2>,
    /// Wi-Fi station RSSI in dBm; `None` if not connected.
    pub wifi_rssi: Option<i32>,
    /// NVS used and total entry counts; `None` if NVS is not initialised.
    pub nvs: Option<(u32, u32)>,
    /// `FreeRTOS` task list.
    pub tasks: heapless::Vec<Task, MAX_TASKS>,
}

/// ESP32 reset reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetReason {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Startup {
    /// Reason the device last reset.
    pub reason: ResetReason,
    /// Chip model name (e.g. `"esp32s3"`).
    pub chip: heapless::String<MAX_STR_LEN>,
    /// Number of CPU cores.
    pub cores: u8,
    /// Silicon revision number.
    pub revision: u16,
    /// Wi-Fi station MAC address bytes.
    pub mac: [u8; 6],
    /// Default flash chip size in bytes.
    pub flash_size: u32,
}

/// Partition type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartType {
    App,
    Data,
    Unknown,
}

/// One partition table entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Partition label.
    pub label: heapless::String<MAX_NAME_LEN>,
    /// Partition type.
    pub part_type: PartType,
    /// Partition start address in flash.
    pub offset: u32,
    /// Partition size in bytes.
    pub size: u32,
}

/// A parsed agent message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Message {
    /// Recurring telemetry frame.
    Frame(Frame),
    /// One-time startup metadata line.
    Startup(Startup),
    /// One-time partition table line.
    Partitions(heapless::Vec<Partition, MAX_PARTITIONS>),
}

// Gated on target_os rather than not(test) because cargo clippy
// --all-targets builds the staticlib for the host (x86_64) as well as the
// embedded targets. A no_std staticlib on x86_64 with panic="unwind" (the
// default) fails to compile; by restricting no_std to the two embedded target
// OS values used here ("none" = bare-metal RISC-V, "espidf" = Xtensa
// ESP-IDF) the host staticlib gets std and its panic machinery, while
// embedded builds remain no_std as required.
#![cfg_attr(any(target_os = "none", target_os = "espidf"), no_std)]

use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(any(target_os = "none", target_os = "espidf"))]
mod ffi;
#[cfg(any(target_os = "none", target_os = "espidf", test))]
mod fmt;
#[cfg(any(target_os = "none", target_os = "espidf"))]
mod task;

#[cfg(any(target_os = "none", target_os = "espidf"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

pub(crate) static AGENT_INTERVAL: AtomicU32 = AtomicU32::new(1000);

/// Overrides the agent sampling interval.
///
/// The constructor always starts the agent task with a default of 1000 ms.
/// Call this from `app_main` to use a different interval. Changes are picked
/// up on the next task iteration. Passing `interval_ms = 0` suspends
/// telemetry indefinitely. Output always goes to stdout (the configured
/// ESP-IDF console). C callers use the `esp_agent_configure` symbol exported
/// with `#[no_mangle]`.
///
/// # Arguments
///
/// * `interval_ms` - Sampling interval in milliseconds.
pub fn configure(interval_ms: u32) {
    AGENT_INTERVAL.store(interval_ms, Ordering::Relaxed);
}

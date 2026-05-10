// Gated on target_os rather than not(test) because cargo clippy
// --all-targets builds the staticlib for the host (x86_64) as well as the
// embedded targets. A no_std staticlib on x86_64 with panic="unwind" (the
// default) fails to compile; by restricting no_std to the two embedded target
// OS values used here ("none" = bare-metal RISC-V, "espidf" = Xtensa
// ESP-IDF) the host staticlib gets std and its panic machinery, while
// embedded builds remain no_std as required.
#![cfg_attr(any(target_os = "none", target_os = "espidf"), no_std)]

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

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

pub(crate) static AGENT_UART: AtomicU8 = AtomicU8::new(0);
pub(crate) static AGENT_INTERVAL: AtomicU32 = AtomicU32::new(1000);

/// Overrides the agent UART port and sampling interval.
///
/// The `.init_array` constructor always starts the agent task with defaults
/// (UART 0, 1000 ms). Call this from `app_main` to use a different port or
/// interval. Changes are picked up on the next task iteration. Passing
/// `interval_ms = 0` suspends telemetry indefinitely. C callers use the
/// `esp_agent_configure` symbol exported with `#[no_mangle]`.
///
/// # Arguments
///
/// * `uart_num` - ESP-IDF UART port number (0, 1, or 2).
/// * `interval_ms` - Sampling interval in milliseconds.
pub fn configure(uart_num: u8, interval_ms: u32) {
    AGENT_UART.store(uart_num, Ordering::Relaxed);
    AGENT_INTERVAL.store(interval_ms, Ordering::Relaxed);
}

use esp_idf_sys as _;

unsafe extern "C" {
    fn esp_agent_configure(interval_ms: u32);
}

fn main() {
    // Optional: overrides the default 1000 ms sampling interval.
    unsafe {
        esp_agent_configure(2000);
    }

    println!("esp_agent running - connect esp-tui to see telemetry");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

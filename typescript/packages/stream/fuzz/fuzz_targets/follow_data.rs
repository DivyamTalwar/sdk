#![no_main]
use libfuzzer_sys::fuzz_target;
use std::{
    path::Path,
    sync::{Mutex, OnceLock},
};
#[path = "../driver.rs"]
mod driver;
#[path = "../follow_data.rs"]
mod oracle;

fuzz_target!(|input: &[u8]| {
    if input.len() > 4096 {
        return;
    }
    static DRIVER: OnceLock<Mutex<driver::Driver>> = OnceLock::new();
    let shared = DRIVER.get_or_init(|| {
        Mutex::new(driver::Driver::configured(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.."),
            "follow-data",
            "",
        ))
    });
    let hex: String = input.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut real = shared.lock().unwrap();
    for delivery in [1, 2] {
        let value = real.call(
            serde_json::json!({"action":"decode", "hex":hex, "delivery":delivery, "from":"0"}),
        );
        oracle::verify(&value, delivery, 0);
    }
});

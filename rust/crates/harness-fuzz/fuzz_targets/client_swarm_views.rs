#![no_main]
#[path = "../support/swarm_views.rs"]
mod oracle;
libfuzzer_sys::fuzz_target!(|bytes: &[u8]| oracle::exercise(bytes, false));

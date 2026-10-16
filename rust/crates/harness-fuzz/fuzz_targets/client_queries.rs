#![no_main]
#[path = "../support/file_client.rs"]
mod file_client;
#[path = "../support/queries.rs"]
mod queries;
libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    queries::exercise(bytes);
    file_client::exercise(bytes);
});

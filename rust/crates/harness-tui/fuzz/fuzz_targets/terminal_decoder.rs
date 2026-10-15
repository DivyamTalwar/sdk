#![no_main]
use acyclic_harness_tui::term::{Environment, PROBE_LIMIT, Probe};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let mut whole = Probe::default();
    whole.feed(bytes);
    whole.finish_input();
    let mut split = Probe::default();
    let chunk = bytes.first().map_or(1, |byte| usize::from(*byte) + 1);
    for bytes in bytes.chunks(chunk) {
        split.feed(bytes);
    }
    split.finish_input();
    assert_eq!(whole.replies, split.replies);
    assert!(whole.replies.input.len() <= PROBE_LIMIT);
    let _ = whole.replies.capabilities(&Environment::default());
});

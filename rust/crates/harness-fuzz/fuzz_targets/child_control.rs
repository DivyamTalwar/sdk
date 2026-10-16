#![no_main]
use acyclic_harness::task_volume::{Control, FileKey, Query};
libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Ok(control) = Control::parse(bytes) {
        assert_eq!(control.bytes(), bytes);
    }
    if let Ok(path) = std::str::from_utf8(bytes) {
        if let Ok(key) = FileKey::parse(path) {
            assert_eq!(key.path(), path);
            assert_eq!(FileKey::parse(&key.path()).unwrap(), key);
            assert!(Query::Root.matches(&key));
        }
    }
});

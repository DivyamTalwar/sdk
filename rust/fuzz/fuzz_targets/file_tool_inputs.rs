//! Exercises the production file-tool and protected file-metadata codecs.
#![no_main]

use acyclic_harness::{conversation::FileRef, tool::files::ReadFileInput};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    if let Ok(input) = serde_json::from_slice::<ReadFileInput>(bytes) {
        let encoded = serde_json::to_vec(&input).unwrap();
        assert_eq!(
            serde_json::from_slice::<ReadFileInput>(&encoded).unwrap(),
            input
        );
    }
    if let Ok(metadata) = serde_json::from_slice::<FileRef>(bytes) {
        if metadata.validate(&mut None).is_ok() {
            let encoded = serde_json::to_vec(&metadata).unwrap();
            assert_eq!(
                serde_json::from_slice::<FileRef>(&encoded).unwrap(),
                metadata
            );
        }
    }
});

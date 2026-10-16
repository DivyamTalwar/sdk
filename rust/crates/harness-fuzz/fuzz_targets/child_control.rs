#![no_main]
use acyclic_harness::task_volume::{ChildRequest, Control, FileKey, Query};
libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Ok(command) =
        serde_json::from_slice::<acyclic_harness::tools::agent::AgentCommand>(bytes)
    {
        let encoded = serde_json::to_vec(&command).unwrap();
        assert_eq!(
            serde_json::from_slice::<acyclic_harness::tools::agent::AgentCommand>(&encoded)
                .unwrap(),
            command
        );
    }
    if let Ok(request) = ChildRequest::parse(bytes, 65536) {
        let encoded = request.encode();
        assert_eq!(
            ChildRequest::parse(&encoded, encoded.len() as u64).unwrap(),
            request
        );
    }
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

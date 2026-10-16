#![no_main]
use acyclic_harness::task_volume::{ChildRequest, ChildStatus, Control, FileKey, Query};
libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    if let Ok(request) = serde_json::from_slice::<acyclic_harness::fork::ForkRequest>(bytes)
        && request.validate(&mut None).is_ok()
    {
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: acyclic_harness::fork::ForkRequest = serde_json::from_slice(&encoded).unwrap();
        decoded.validate(&mut None).unwrap();
        assert_eq!(decoded, request);
    }
    if let Ok(seed) = serde_json::from_slice::<acyclic_harness::fork::ForkSeed>(bytes)
        && seed.validate(&mut None).is_ok()
    {
        let encoded = serde_json::to_vec(&seed).unwrap();
        let decoded: acyclic_harness::fork::ForkSeed = serde_json::from_slice(&encoded).unwrap();
        decoded.validate(&mut None).unwrap();
        assert_eq!(decoded, seed);
    }
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
    if let Ok(status) = ChildStatus::parse(bytes) {
        assert_eq!(
            ChildStatus::parse(&serde_json::to_vec(&status).unwrap()).unwrap(),
            status
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

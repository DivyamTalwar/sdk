#![no_main]
use acyclic_harness::{
    client::CommandResolution,
    model::{FileProjectionPolicy, ModelContent, ModelContentPart},
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    let Ok(resolution) = serde_json::from_slice::<CommandResolution>(data) else {
        return;
    };
    let encoded = serde_json::to_vec(&resolution).unwrap();
    assert_eq!(
        serde_json::from_slice::<CommandResolution>(&encoded).unwrap(),
        resolution
    );
    if let Ok(content) = resolution.into_model_input() {
        content.validate_user_input(&mut None).unwrap();
        for part in content.parts() {
            if let ModelContentPart::File { policy, .. } = part {
                assert_eq!(policy, &FileProjectionPolicy::BoundedFull);
            }
        }
        let json = serde_json::to_vec(&content).unwrap();
        assert_eq!(
            serde_json::from_slice::<ModelContent>(&json).unwrap(),
            content
        );
    }
});

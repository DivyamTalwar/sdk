#![no_main]

use acyclic_harness::OperationId;
use libfuzzer_sys::fuzz_target;
#[path = "../../src/distributed/origin_sequence.rs"]
mod origin_sequence;
#[path = "../../src/executor/summary_identity.rs"]
mod summary_identity;

fuzz_target!(|input: &[u8]| {
    if let Some(sequence) = origin_sequence::decode(input) {
        assert_eq!(input, sequence.to_le_bytes());
    } else {
        assert_ne!(input.len(), 8);
    }
    if input.len() > 4096 {
        return;
    }
    if let Ok(path) = std::str::from_utf8(input) {
        if let Some(operation) = summary_identity::summary_operation(path) {
            let retained = format!(".system/execution/{operation}/{}.json", "0".repeat(64));
            assert_eq!(
                summary_identity::summary_operation(&retained),
                Some(operation)
            );
        }
    }
    if let Ok(summary) = serde_json::from_slice::<acyclic_harness::context::ContextSummary>(input) {
        if summary.output.validate(&mut None).is_ok() {
            let encoded = serde_json::to_vec(&summary).unwrap();
            assert_eq!(
                serde_json::from_slice::<acyclic_harness::context::ContextSummary>(&encoded)
                    .unwrap(),
                summary
            );
        }
    }
});

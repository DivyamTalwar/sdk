#![cfg(all(feature = "filesystem", not(target_arch = "wasm32")))]

#[path = "../fuzz/fuzz_targets/recorded_invocation_input.rs"]
mod input;
use acyclic_harness::{OperationId, conversation::Limits, tool::ToolInvocation};
use input::Record;
use proptest::prelude::*;
use serde_json::{Value, json};

fn record(arguments: Value, metadata: Value) -> Record {
    Record {
        invocation: ToolInvocation {
            operation_id: OperationId::from_bytes([7; 16]),
            call_id: "call".into(),
            name: "echo".into(),
            arguments,
        },
        metadata,
    }
}

#[test]
fn valid_record_is_admitted_and_projects_with_exact_metadata() {
    let record = record(
        json!({"path":"λ\0🦀"}),
        json!({"signature":"opaque", "blocks":[{"thinking":"plan"}]}),
    );
    assert!(
        record
            .invocation
            .validate_recorded(&record.metadata, Limits::default(), None)
            .is_ok()
    );
    input::assert_replay(record, Limits::default());
}

#[test]
fn complete_depth_and_numeric_bounds_apply_before_recording() {
    let mut deep = json!(0);
    for _ in 0..127 {
        deep = json!([deep]);
    }
    for record in [
        record(deep, Value::Null),
        record(json!({}), json!(u64::MAX)),
        record(json!({"value":u64::MAX}), Value::Null),
    ] {
        assert!(
            record
                .invocation
                .validate_recorded(&record.metadata, Limits::default(), None)
                .is_err()
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    // Property: ordinary/native admission agrees, releases all temporary custody,
    // and every admitted complete record survives actual history projection/replay.
    #[test]
    fn admitted_records_always_project_and_replay(
        text in ".{0,96}", signature in ".{0,96}", depth in 0usize..140,
        file_bytes in 128u64..4096, render_bytes in 128u64..4096,
    ) {
        let mut nested = json!(0);
        for _ in 0..depth { nested = json!([nested]); }
        let record = record(json!({"data":text, "nested":nested}), json!({"signature":signature}));
        let limits = Limits {file_bytes, render_bytes, ..Limits::default()};
        let admitted = record.invocation.validate_recorded(&record.metadata, limits, None).is_ok();
        let work = acyclic_fs::NativeMaterializationWork::new(
            acyclic_fs::performance::work_budget(2_000_000, 4*1024*1024),
            &acyclic_fs::CancellationToken::new(),
        ).expect("fixture ledger");
        let before = work.live_allocation_bytes();
        let io = acyclic_harness::filesystem::PreparationIo::from_native_work(&work).unwrap();
        let native = record.invocation.validate_recorded(&record.metadata, limits, Some(&io));
        prop_assert_eq!(admitted, native.is_ok());
        drop(native);
        drop(io);
        prop_assert_eq!(before, work.live_allocation_bytes());
        input::assert_replay(record, limits);
    }
}

#[test]
fn checked_in_saved_record_corpus() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/recorded_invocation");
    for entry in std::fs::read_dir(directory).unwrap() {
        input::check(&std::fs::read(entry.unwrap().path()).unwrap());
    }
}

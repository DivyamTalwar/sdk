use super::*;
use crate::conversation::{
    BoundedContentFuture, FileDescriptor, FileRef, VolumeClass, VolumeOwner, VolumeRef,
};
use crate::{AgentId, resources::ProviderRef};
use proptest::prelude::*;
use serde_json::{Value, json};

struct Resolver(Vec<u8>);
impl ContentResidencyVerifier for Resolver {
    fn read<'a>(
        &'a self,
        _: &'a FileRef,
        _: u64,
        io: Option<&'a PreparationIo>,
    ) -> Result<BoundedContentFuture<'a, acyclic_stream::OwnedBytes>> {
        BoundedContentFuture::new(io, || async {
            Ok(acyclic_stream::OwnedBytes::copy_from_slice(&self.0))
        })
    }
}

fn reference(bytes: &[u8]) -> FileRef {
    FileRef::new(
        VolumeRef::new(
            ProviderRef::new("fixture".into(), "filesystem".into(), "2".into(), &mut None).unwrap(),
            "private".into(),
            VolumeClass::AgentPrivate,
            VolumeOwner::Agent(AgentId::from_bytes([1; 16])),
            &mut None,
        )
        .unwrap(),
        "invocation.json".into(),
        "v1".into(),
        FileDescriptor::from_bytes(bytes, "application/json", &mut None).unwrap(),
        "fixture".into(),
        &mut None,
    )
    .unwrap()
}

fn check_rejected(value: Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    let reference = reference(&bytes);
    let resolver = Resolver(bytes);
    let work = acyclic_fs::NativeMaterializationWork::new(
        acyclic_fs::performance::work_budget(2_000_000, 4 * 1024 * 1024),
        &acyclic_fs::CancellationToken::new(),
    )
    .unwrap();
    let io = PreparationIo::from_native_work(&work).unwrap();
    let baseline = work.live_allocation_bytes();
    let result = futures::executor::block_on(artifacts::read_tool_call(
        &resolver,
        &reference,
        4096,
        Some(&io),
    ));
    assert!(result.is_err());
    drop(result);
    // Check while the I/O scope is still alive: scope destruction must not hide a leak.
    assert_eq!(work.live_allocation_bytes(), baseline);
    assert_eq!(io.owned_bytes().unwrap(), 0);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn every_rejected_record_releases_all_source_and_field_custody(
        text in ".{0,128}", malformed in 0usize..8, nesting in 0usize..8,
    ) {
        let mut value = json!({"invocation":{"call_id":"call", "name":"echo", "arguments":{"payload":text}},"metadata":{"signature":text}});
        match malformed {
            0 => { value = json!([value]); },
            1 => {value.as_object_mut().unwrap().remove("metadata");},
            2 => {value.as_object_mut().unwrap().remove("invocation");},
            3 => {value["invocation"] = json!([text]);},
            4 => {value["invocation"]["call_id"] = Value::Null;},
            5 => {value["invocation"]["name"] = json!(7);},
            6 => {value["invocation"].as_object_mut().unwrap().remove("arguments");},
            _ => {value["metadata"]["unsafe"] = json!(u64::MAX);},
        }
        for _ in 0..nesting {value = json!({"wrapper":value});}
        check_rejected(value);
    }
}

#[test]
fn removed_metadata_rejection_does_not_leave_the_decoded_record_retained() {
    check_rejected(
        json!({"invocation":{"call_id":"call", "name":"echo", "arguments":{"payload":"x".repeat(1000)}}, "extra":"y".repeat(1000)}),
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn guarded_splits_and_joins_conserve_the_original_ledger(bytes in 0u64..8192, fraction in any::<u16>()) {
        let work = acyclic_fs::NativeMaterializationWork::new(
            acyclic_fs::performance::work_budget(100_000, 4*1024*1024),
            &acyclic_fs::CancellationToken::new(),
        ).unwrap();
        let io = PreparationIo::from_native_work(&work).unwrap();
        let baseline = work.live_allocation_bytes();
        let mut hold = io.reserve_decode(bytes).unwrap();
        let part = bytes * u64::from(fraction) / u64::from(u16::MAX);
        let split = hold.split_off(part).unwrap();
        prop_assert_eq!(hold.bytes() + split.bytes(), bytes);
        prop_assert_eq!(work.live_allocation_bytes() - baseline, bytes);
        hold.absorb(split).unwrap();
        prop_assert_eq!(hold.bytes(), bytes);
        prop_assert!(hold.split_off(bytes+1).is_err());
        prop_assert_eq!(hold.bytes(), bytes);
        drop(hold);
        prop_assert_eq!(work.live_allocation_bytes(), baseline);
    }
}

#[test]
fn premature_retention_negative_control_is_visible_before_io_scope_drop() {
    let bytes = br#"{"invocation":{"arguments":{"payload":"charged"}}}"#;
    let file = reference(bytes);
    let resolver = Resolver(bytes.to_vec());
    let work = acyclic_fs::NativeMaterializationWork::new(
        acyclic_fs::performance::work_budget(100_000, 4 * 1024 * 1024),
        &acyclic_fs::CancellationToken::new(),
    )
    .unwrap();
    let io = PreparationIo::from_native_work(&work).unwrap();
    let baseline = work.live_allocation_bytes();
    let (value, hold) =
        futures::executor::block_on(artifacts::read_value(&resolver, &file, 4096, Some(&io)))
            .unwrap();
    // Broken original ordering: release the guard before discovering missing metadata.
    hold.unwrap().into_retained().unwrap();
    assert!(value.get("metadata").is_none());
    drop(value);
    assert_ne!(work.live_allocation_bytes(), baseline);
    io.release(io.owned_bytes().unwrap()).unwrap();
    assert_eq!(work.live_allocation_bytes(), baseline);
}

//! Original public paths retain the leaf crate's exact types.

#[test]
fn facade_reexports_the_protocol_without_wrapping_types() {
    let _: acyclic_harness_protocol::wire::EventEnvelope =
        acyclic_harness::wire::EventEnvelope::default();
    let _: acyclic_harness::wire::ProtocolIdentity =
        acyclic_harness_protocol::wire::ProtocolIdentity::default();
    assert_eq!(
        acyclic_harness::FILE_DESCRIPTOR_SET,
        acyclic_harness_protocol::FILE_DESCRIPTOR_SET,
    );
}

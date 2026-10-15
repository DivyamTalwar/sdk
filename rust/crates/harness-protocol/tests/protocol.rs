//! Canonical wire compatibility across crate boundaries.

use acyclic_harness_protocol::FILE_DESCRIPTOR_SET;

#[test]
fn descriptor_matches_the_published_v2_protocol() {
    assert_eq!(
        blake3::hash(FILE_DESCRIPTOR_SET).to_hex().as_str(),
        "7a22a03e37cb830a329a22e11b1691b631a34f36ea997aee99dabb591c4b8c57"
    );
}

//! N-07 / ANT-TOOL-03 / OAR-TOOL-02 / GEM-TOOL-03.
//! History may contain non-object arguments; preserve their value in an object.
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;

/// Borrows the original value until the final wire serialization. No intermediate
/// argument tree is allocated, and already-portable objects remain unchanged.
pub(super) struct Arguments<'a>(pub &'a Value);

impl Serialize for Arguments<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.is_object() {
            self.0.serialize(serializer)
        } else {
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry("_raw", self.0)?;
            map.end()
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "fuzzing")))]
pub(crate) fn assert_normalization(value: &Value) {
    let bytes = serde_json::to_vec(&Arguments(value)).unwrap();
    let normalized: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(normalized.is_object());
    assert_eq!(
        if value.is_object() {
            &normalized
        } else {
            &normalized["_raw"]
        },
        value
    );
    assert_eq!(serde_json::to_vec(&Arguments(&normalized)).unwrap(), bytes);
}

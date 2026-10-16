//! Synchronization selected only by the dedicated model command's cfg.
#[cfg(acyclic_harness_loom)]
pub(crate) use loom::sync::{Arc, atomic};
#[cfg(not(acyclic_harness_loom))]
pub(crate) use std::sync::{Arc, atomic};

#[cfg(all(
    feature = "native-execution",
    feature = "filesystem-local",
    not(target_arch = "wasm32")
))]
#[path = "sync/owned.rs"]
pub(crate) mod owned;

#[cfg(all(test, not(acyclic_harness_loom)))]
#[test]
fn ordinary_build_uses_standard_synchronization() {
    assert!(!cfg!(acyclic_harness_loom));
    let value: std::sync::Arc<atomic::AtomicUsize> = Arc::new(atomic::AtomicUsize::new(0));
    assert_eq!(value.load(atomic::Ordering::Relaxed), 0);
}

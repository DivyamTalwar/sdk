#[cfg(acyclic_fs_loom)]
pub(crate) use loom::sync::Mutex;
#[cfg(not(acyclic_fs_loom))]
pub(crate) use std::sync::Mutex;

#[cfg(all(test, feature = "model-checking", not(acyclic_fs_loom)))]
mod tests {
    #[test]
    fn model_feature_alone_keeps_the_production_mutex() {
        // Type identity makes feature-driven substitution a compilation failure;
        // ordinary execution also checks it never needs a Loom model context.
        let mutex: std::sync::Mutex<u8> = super::Mutex::new(0);
        *mutex.lock().expect("production lock") = 1;
        assert_eq!(*mutex.lock().expect("production lock"), 1);
    }
}

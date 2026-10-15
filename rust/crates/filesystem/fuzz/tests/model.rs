//! Exhaustive schedules through the production native directory cache.
use acyclic_fs::{Digest, ObjectId, ObjectKind};

fn key(byte: u8) -> ObjectId {
    ObjectId {
        kind: ObjectKind::TreePage,
        digest: Digest::from_bytes([byte; 32]),
    }
}

#[path = "../../src/native_mount/adapter/folded_cache.rs"]
mod cache;

mod view;

#[test]
fn immutable_directory_results_are_never_torn_or_lost() {
    loom::model(|| {
        let cache = loom::sync::Arc::new(cache::FoldedDirectoryCache::new());
        let writer = loom::sync::Arc::clone(&cache);
        let thread = loom::thread::spawn(move || {
            writer.insert(key(1), true);
            writer.insert(key(2), false);
        });
        cache.insert(key(3), true);
        assert!(cache.get(key(1)).is_none_or(|value| value));
        assert!(cache.get(key(2)).is_none_or(|value| !value));
        assert_eq!(cache.get(key(4)), None);
        thread.join().expect("writer completes without deadlock");
        assert_eq!(cache.get(key(1)), Some(true));
        assert_eq!(cache.get(key(2)), Some(false));
        assert_eq!(cache.get(key(3)), Some(true));
    });
}

#[test]
fn eviction_and_duplicate_admission_keep_other_results_intact() {
    loom::model(|| {
        let cache = loom::sync::Arc::new(cache::FoldedDirectoryCache::new());
        for index in 0..64 {
            cache.insert(key(index), index % 2 == 0);
        }
        let writer = loom::sync::Arc::clone(&cache);
        let thread = loom::thread::spawn(move || {
            writer.insert(key(65), true);
            writer.insert(key(65), true);
        });
        cache.insert(key(66), false);
        assert_eq!(cache.get(key(63)), Some(false));
        thread.join().expect("writer completes without deadlock");
        assert_eq!(cache.get(key(0)), None);
        assert_eq!(cache.get(key(1)), None);
        assert_eq!(cache.get(key(2)), Some(true));
        assert_eq!(cache.get(key(65)), Some(true));
        assert_eq!(cache.get(key(66)), Some(false));
    });
}

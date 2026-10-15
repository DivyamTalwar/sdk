#[cfg(loom)]
use loom::sync::Mutex;
#[cfg(not(loom))]
use std::sync::Mutex;

// Bounded, disposable cache of immutable directory-tree identities. Namespace
// edits produce a different tree id, so no mutable-view invalidation is needed.
const SLOTS: usize = 64;
use crate::ObjectId;

pub(super) struct FoldedDirectoryCache {
    // Lock order: view gate -> shared checkout -> this terminal lock. No guard
    // escapes; no allocation, accounting, callbacks or other locks occur inside.
    state: Mutex<(usize, [Option<(ObjectId, bool)>; SLOTS])>,
}

impl FoldedDirectoryCache {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new((0, [None; SLOTS])),
        }
    }

    pub(super) fn get(&self, key: ObjectId) -> Option<bool> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .iter()
            .flatten()
            .find_map(|(tree, unique)| (*tree == key).then_some(*unique))
    }

    pub(super) fn insert(&self, key: ObjectId, unique: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.1.iter().flatten().any(|(tree, _)| *tree == key) {
            return;
        }
        let position = state.0;
        state.1[position] = Some((key, unique));
        state.0 = (position + 1) % SLOTS;
    }

    // Negative control: two writers can reserve the same slot when selection
    // and publication do not share the production critical section.
    #[cfg(all(test, loom))]
    pub(super) fn insert_split(&self, key: ObjectId, unique: bool) {
        let position = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.1[position] = Some((key, unique));
        state.0 = (position + 1) % SLOTS;
    }
}

use super::{InvalidPolicy, PathTrie};
#[cfg(loom)]
use loom::sync::RwLock;
use std::sync::Arc;
#[cfg(not(loom))]
use std::sync::RwLock;

/// Published compiled access for one authenticated mount binding.
/// The trusted policy owner installs revisions; new operations load the latest
/// view, while open handles retain their original immutable view.
pub struct AccessView {
    // Terminal lock: callers never receive a guard. Compilation, allocation,
    // destruction and backend operations run outside it; it acquires no locks.
    current: RwLock<Arc<PathTrie>>,
}

impl AccessView {
    /// Bind an already authenticated policy/capability intersection.
    #[must_use]
    pub fn new(view: PathTrie) -> Self {
        Self {
            current: RwLock::new(Arc::new(view)),
        }
    }

    /// Load the current view without allocating or consulting storage.
    pub fn current(&self) -> Result<Arc<PathTrie>, InvalidPolicy> {
        self.current
            .read()
            .map(|view| Arc::clone(&view))
            .map_err(|_| InvalidPolicy("access view publication failed"))
    }

    /// Publish a revision after the owner authorizes and commits its policy edit.
    /// Binding changes and older policy revisions fail closed.
    pub fn publish(&self, view: PathTrie) -> Result<(), InvalidPolicy> {
        let view = Arc::new(view);
        let mut current = self
            .current
            .write()
            .map_err(|_| InvalidPolicy("access view publication failed"))?;
        if !view.same_binding(&current) || view.policy_version() < current.policy_version() {
            return Err(InvalidPolicy("access view binding or revision changed"));
        }
        let previous = std::mem::replace(&mut *current, view);
        drop(current);
        drop(previous);
        Ok(())
    }
}

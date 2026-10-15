use super::{InvalidPolicy, PathTrie};
use std::sync::{Arc, RwLock};

/// Published compiled access for one authenticated mount binding.
/// The trusted policy owner installs revisions; new operations load the latest
/// view, while open handles retain their original immutable view.
pub struct AccessView {
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
        let mut current = self
            .current
            .write()
            .map_err(|_| InvalidPolicy("access view publication failed"))?;
        if view.config() != current.config()
            || view.volume() != current.volume()
            || view.subject() != current.subject()
            || view.capability_binding != current.capability_binding
            || view.policy_version() < current.policy_version()
        {
            return Err(InvalidPolicy("access view binding or revision changed"));
        }
        *current = Arc::new(view);
        Ok(())
    }
}

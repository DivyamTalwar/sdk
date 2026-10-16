use super::sync::{Arc, Mutex};
use crate::{Error, Result};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::PoisonError,
};

pub(super) struct Registry<H: ?Sized> {
    // Lock order: registry only. No user code runs under the lock; lookup
    // returns an owned host before dispatch or any lifetime/listener access.
    hosts: Mutex<BTreeMap<String, Arc<H>>>,
}

impl<H: ?Sized> Registry<H> {
    pub fn new() -> Self {
        Self {
            hosts: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, workspace: String, host: Arc<H>) -> Result<()> {
        match self
            .hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace)
        {
            Entry::Occupied(_) => Err(Error::Conflict("workspace already registered".into())),
            Entry::Vacant(entry) => {
                entry.insert(host);
                Ok(())
            }
        }
    }

    pub fn get(&self, workspace: &str) -> Result<Arc<H>> {
        self.hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .cloned()
            .ok_or_else(|| Error::NotFound("workspace".into()))
    }
}

#[cfg(all(test, acyclic_harness_loom))]
#[path = "registry/models.rs"]
mod models;

#[cfg(loom)]
pub(super) use loom::sync::{Arc, Mutex};
#[cfg(not(loom))]
pub(super) use std::sync::{Arc, Mutex};

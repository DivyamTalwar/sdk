#![cfg(loom)]
//! Compile the actual lifecycle with its instrumented mutex, keeping dependency
//! implementations out of the model. Run with cargo rustc ... -- --cfg loom.

use acyclic_harness::{Error, Result};

#[path = "../src/service/lifetime.rs"]
mod lifetime;
#[path = "../src/service/registry.rs"]
mod registry;
#[path = "../src/service/sync.rs"]
mod sync;

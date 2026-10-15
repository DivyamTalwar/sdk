//! Deterministic model scripts for harness consumers and scenario tests.

mod scripted;

pub use scripted::{ScriptedEvent, ScriptedModelProvider, ScriptedTurn};

#[cfg(test)]
mod tests;

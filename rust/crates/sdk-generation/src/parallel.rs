#[cfg(acyclic_sdk_docs_loom)]
use loom::{sync::Arc, thread};
#[cfg(not(acyclic_sdk_docs_loom))]
use std::{sync::Arc, thread};

// Callers provide disjoint outputs. No lock is held while spawning or joining;
// worker initialization uses the existing leaf slots. Join every worker before
// exposing any result, including errors and panics, to the publication path.
pub fn map<T, R, F>(inputs: Vec<T>, operation: F) -> Vec<R>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(T) -> R + Send + Sync + 'static,
{
    let operation = Arc::new(operation);
    let jobs = inputs
        .into_iter()
        .map(|input| {
            let operation = Arc::clone(&operation);
            thread::spawn(move || operation(input))
        })
        .collect::<Vec<_>>();
    let joined = jobs.into_iter().map(|job| job.join()).collect::<Vec<_>>();
    joined
        .into_iter()
        .map(|result| result.unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
        .collect()
}

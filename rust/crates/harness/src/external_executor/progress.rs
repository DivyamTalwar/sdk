//! Passive requests recheck the state after establishing their push follow.
use crate::{Error, Result};
use futures::{Stream, StreamExt};

pub(crate) async fn follow<
    T,
    F: Future<Output = Result<Option<T>>>,
    S: Stream<Item = Result<()>> + Unpin,
>(
    mut step: impl FnMut() -> F,
    subscribe: impl Future<Output = Result<S>>,
    stopped: impl Future<Output = Result<()>>,
) -> Result<Option<T>> {
    if let Some(result) = step().await? {
        return Ok(Some(result));
    }
    let mut changes = subscribe.await?;
    tokio::pin!(stopped);
    loop {
        // The task can become ready between the first snapshot and this cut.
        if let Some(result) = step().await? {
            return Ok(Some(result));
        }
        tokio::select! {
            biased;
            stopped = &mut stopped => {stopped?; return Ok(None);}
            change = changes.next() => {
                change.ok_or_else(|| Error::Storage("workflow follow ended".into()))??;
            }
        }
    }
}

#[cfg(all(test, not(acyclic_harness_loom)))]
mod tests;

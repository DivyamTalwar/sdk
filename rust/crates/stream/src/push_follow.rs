//! One push wake bridge for cached provider views, shared with the loom model.
use crate::{BoxProviderFuture, CommitId, ProviderTask, RecordStream, StreamError, query_sync::watch};
use futures::{FutureExt as _, StreamExt as _};

pub(crate) fn following<F>(
    records: RecordStream,
    wake: watch::Receiver<()>,
    next: u64,
    anchor: Option<CommitId>,
    refresh: F,
) -> RecordStream
where F: FnMut(u64, Option<CommitId>) -> BoxProviderFuture<'static, Result<RecordStream, StreamError>> + ProviderTask + 'static,
{
    let followed = futures::stream::try_unfold((records, wake, next, anchor, refresh),
        |(mut records, mut wake, mut next, mut anchor, mut refresh)| async move {
            loop {
                futures::select_biased! {
                    changed = wake.changed().fuse() => {
                        changed.map_err(|_| StreamError::Unavailable)?;
                        records = refresh(next, anchor).await?;
                    },
                    record = records.next().fuse() => {
                        let record = record.ok_or(StreamError::Unavailable)??;
                        if record.sequence != next { return Err(StreamError::PrefixNotRetained); }
                        next = next.checked_add(1).ok_or(StreamError::LimitExceeded)?;
                        anchor = Some(record.commit_id);
                        return Ok(Some((record, (records, wake, next, anchor, refresh))));
                    },
                }
            }
        });
    #[cfg(not(target_arch = "wasm32"))]
    { followed.boxed() }
    #[cfg(target_arch = "wasm32")]
    { followed.boxed_local() }
}

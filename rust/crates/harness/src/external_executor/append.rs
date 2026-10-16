use crate::task_volume::{Error, FileValue, Query, Result, TaskVolume};
use acyclic_stream::StreamProvider;

/// Publishes one immutable request, rebasing only on accepted concurrent writes.
pub(crate) async fn publish<P: StreamProvider + ?Sized>(
    requests: &TaskVolume<P>,
    file: &FileValue,
) -> Result<()> {
    loop {
        let snapshot = requests.snapshot(&Query::File(file.key.clone()), 1).await?;
        if let Some(original) = snapshot.files.first() {
            return if original == file {
                Ok(())
            } else {
                Err(Error::ImmutableConflict(file.key.clone()))
            };
        }
        match requests.append(snapshot.generation, file).await {
            Ok(_) => return Ok(()),
            // Every retry requires actual accepted progress, never a timer or
            // a repeated check of unchanged task state.
            Err(Error::Conflict(next))
                if next.volume == snapshot.generation.volume
                    && next.epoch == snapshot.generation.epoch
                    && next.position > snapshot.generation.position => {}
            Err(error) => return Err(error),
        }
    }
}

use crate::{
    Error, Result,
    task_volume::{FileValue, Query, TaskVolume},
};
use acyclic_stream::{CommitCondition, StreamProvider};

// The owner condition and the volume head must be checked by the same provider
// transaction. No lock or retry loop spans these independent observations.
pub(super) async fn publish<P: StreamProvider + ?Sized>(
    volume: &TaskVolume<P>,
    file: &FileValue,
    condition: CommitCondition,
    deadline: Option<u64>,
) -> Result<()> {
    let snapshot = volume
        .snapshot(&Query::File(file.key.clone()), 1)
        .await
        .map_err(storage)?;
    if let Some(existing) = snapshot.files.first() {
        return if existing == file {
            Ok(())
        } else {
            Err(Error::Conflict(
                "task request publication changed on retry".into(),
            ))
        };
    }
    volume
        .append_if(snapshot.generation, file, vec![condition], deadline)
        .await
        .map_err(storage)?;
    Ok(())
}

pub(super) fn storage(error: crate::task_volume::Error) -> Error {
    Error::Storage(error.to_string())
}

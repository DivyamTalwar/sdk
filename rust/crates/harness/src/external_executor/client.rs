use super::{ExternalRequest, volume};
use crate::task_volume::{
    Component, Error, FileKey, FileValue, Query, Result, TaskFile, TaskVolume,
};
use acyclic_stream::StreamProvider;
use futures::StreamExt as _;
use serde_json::Value;

impl ExternalRequest {
    /// Publishes the original request and follows its immutable answer.
    /// Authentication and transport belong to the supplied platform provider.
    /// A conflicting publication returns to the caller without a retry loop.
    pub fn send<'a, P: StreamProvider + ?Sized, R: StreamProvider + ?Sized>(
        &'a self,
        requests: &'a TaskVolume<P>,
        results: &'a TaskVolume<R>,
        task: Component,
    ) -> impl Future<Output = Result<Value>> + 'a {
        Box::pin(async move {
            let write = self.to_file(task.clone())?;
            let snapshot = requests
                .snapshot(&Query::File(write.key.clone()), 1)
                .await?;
            if let Some(existing) = snapshot.files.first() {
                if existing != &write {
                    return Err(Error::ImmutableConflict(write.key));
                }
            } else {
                requests.append(snapshot.generation, &write).await?;
            }
            let key = self.result_key(task)?;
            let mut observation = results.observe(Query::File(key.clone()), None, 1).await?;
            if let Some(file) = observation
                .snapshot
                .and_then(|snapshot| snapshot.files.into_iter().next())
            {
                return volume::decode(&file.bytes);
            }
            while let Some(change) = observation.changes.next().await {
                if let Some(file) = change?.file {
                    if file.key != key || file.channel.is_some() {
                        return Err(Error::InvalidSegment);
                    }
                    return volume::decode(&file.bytes);
                }
            }
            Err(Error::ResetRequired)
        })
    }

    /// Standard result-file identity; only the fenced harness owner may write it.
    pub fn result_key(&self, task: Component) -> Result<FileKey> {
        let mut name = [0; 36];
        let operation = uuid::Uuid::from_bytes(self.operation_id.into_bytes());
        Ok(FileKey::Task {
            task,
            file: TaskFile::RequestResult(Component::new(
                operation.hyphenated().encode_lower(&mut name),
            )?),
        })
    }

    /// Encodes one native hook answer for publication with the actor checkpoint.
    pub fn result_file(&self, task: Component, answer: &Value) -> Result<FileValue> {
        if !self.valid() {
            return Err(Error::InvalidSegment);
        }
        Ok(FileValue::new(
            self.result_key(task)?,
            volume::encode(answer)?,
        ))
    }
}

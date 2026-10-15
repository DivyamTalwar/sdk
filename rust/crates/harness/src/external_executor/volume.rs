use super::ExternalRequest;
use crate::{
    JsonBounds,
    task_volume::{Component, Error, FileKey, FileValue, Generation, Result, TaskFile, TaskVolume},
};
use acyclic_stream::StreamProvider;

// Writes and reads share the bounded JSON decoder's structural ceiling.
const BOUNDS: JsonBounds = JsonBounds {
    maximum_bytes: acyclic_stream::MAX_RECORD_BYTES as u64,
    maximum_items: acyclic_stream::MAX_RECORD_BYTES as u64 + 1,
    maximum_depth: 256,
};

impl ExternalRequest {
    /// Publishes one immutable request through the volume's authorized Streams provider.
    /// Identical retries reuse admission; conflicting bodies retain the volume's typed error.
    pub async fn submit<P: StreamProvider + ?Sized>(
        &self,
        volume: &TaskVolume<P>,
        task: Component,
    ) -> Result<Generation> {
        if !self.valid() {
            return Err(Error::InvalidSegment);
        }
        let bytes = crate::json_bytes_bounded(self, BOUNDS, None, &mut |_| Ok(()))
            .map_err(|_| Error::LimitExceeded)?;
        let mut name = [0; 36];
        let operation = uuid::Uuid::from_bytes(self.operation_id.into_bytes());
        let request = Component::new(operation.hyphenated().encode_lower(&mut name))?;
        volume.submit(task, request, bytes.into()).await
    }

    /// Decodes a request belonging to this task, checking both file and hook identities.
    pub fn from_file(expected_task: &Component, file: &FileValue) -> Result<Self> {
        let FileKey::Task {
            task,
            file: TaskFile::Request(request),
        } = &file.key
        else {
            return Err(Error::InvalidSegment);
        };
        if task != expected_task || file.channel.is_some() {
            return Err(Error::InvalidSegment);
        }
        let decoded: Self = crate::json_from_slice_bounded(
            std::marker::PhantomData::<Self>,
            &file.bytes,
            BOUNDS,
            file.bytes.len() as u64,
            None,
            None,
            &mut |_| Ok(()),
        )
        .map_err(|_| Error::InvalidSegment)?;
        let mut name = [0; 36];
        let operation = uuid::Uuid::from_bytes(decoded.operation_id.into_bytes());
        if request.as_str() != operation.hyphenated().encode_lower(&mut name) || !decoded.valid() {
            return Err(Error::InvalidSegment);
        }
        Ok(decoded)
    }
}

//! Tool output is replayable task files, published under the current task fence.
use super::*;
use crate::{
    durable_host::TaskJournalOwner,
    task_volume::{Component, FileKey, Generation, Query, TaskFile, TaskVolume, ToolFile, Write},
};
use acyclic_native_runtime::ProcessOutput;
use acyclic_stream::OwnedBytes;

pub(super) struct Output {
    pub(super) owner: Arc<TaskJournalOwner<LocalStream>>,
    volume: Arc<TaskVolume<LocalStream>>,
    generation: Generation,
    task: Component,
    call: Component,
    sequence: [u64; 2],
    runtime: tokio::runtime::Handle,
}

impl Output {
    pub(super) async fn open(
        context: &ToolContext,
        volume: Arc<TaskVolume<LocalStream>>,
        request: Vec<u8>,
    ) -> Result<Self> {
        let owner = context.task().ordinary_journal_owner::<LocalStream>()?;
        let task = Component::new(owner.task_id().to_string())?;
        // Runtime operation IDs are canonical and unique across model turns.
        // The request retains the provider's unrestricted call_id unchanged.
        let call = Component::new(context.operation_id().to_string())?;
        let generation = volume
            .snapshot(
                &Query::Tool {
                    task: task.clone(),
                    turn: 0,
                    call: call.clone(),
                },
                0,
            )
            .await?
            .generation;
        let mut output = Self {
            owner,
            volume,
            generation,
            task,
            call,
            sequence: [0; 2],
            runtime: tokio::runtime::Handle::current(),
        };
        output.publish(ToolFile::Request, request.into()).await?;
        Ok(output)
    }

    async fn publish(&mut self, file: ToolFile, bytes: OwnedBytes) -> Result<()> {
        let condition = self
            .owner
            .condition(crate::distributed::JournalWrite::Fresh, &mut None)
            .await?;
        let write = Write::new(
            FileKey::Task {
                task: self.task.clone(),
                file: TaskFile::Tool {
                    turn: 0,
                    call: self.call.clone(),
                    file,
                },
            },
            bytes,
        );
        self.generation = self
            .volume
            .append_if(
                self.generation,
                &write,
                vec![condition],
                self.owner.deadline_epoch_ms(),
            )
            .await?;
        Ok(())
    }

    pub(super) fn observe(&mut self, chunk: ProcessOutput<'_>) -> std::io::Result<()> {
        let (channel, bytes) = match chunk {
            ProcessOutput::Stdout(bytes) => (0, bytes),
            ProcessOutput::Stderr(bytes) => (1, bytes),
        };
        let sequence = self.sequence[channel];
        let file = if channel == 0 {
            ToolFile::Stdout(sequence)
        } else {
            ToolFile::Stderr(sequence)
        };
        let runtime = self.runtime.clone();
        runtime
            .block_on(self.publish(file, bytes.to_vec().into()))
            .map_err(std::io::Error::other)?;
        self.sequence[channel] = sequence
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("tool output sequence overflow"))?;
        Ok(())
    }

    pub(super) async fn finish(&mut self, result: &ToolResult) -> Result<()> {
        self.publish(
            ToolFile::Result,
            serde_json::to_vec(&result.value).map_err(storage)?.into(),
        )
        .await
    }
}

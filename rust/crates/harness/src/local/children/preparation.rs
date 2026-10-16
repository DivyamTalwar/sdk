use super::*;
use crate::{
    filesystem::{FilesystemForkPreparer, PreparationIo, TaskCommandHost, TaskCommandProgress},
    resource_preparation::ResourcePreparationRequest,
    runtime::{TaskContext, TaskStateProvider},
};
use acyclic_stream::BoxProviderFuture;
use serde_json::Value;

pub(super) struct Commands<'a> {
    pub harness: &'a LocalHarness,
    pub source: &'a workspace::View,
    pub cut: acyclic_fs::LazySnapshotRef,
    pub preparer: &'a FilesystemForkPreparer<LocalAuthorityBackend, LocalObjectBackend>,
    pub request: &'a ResourcePreparationRequest,
    pub workspace: &'a LocalWorkspace,
    pub io: &'a PreparationIo,
}

impl Commands<'_> {
    async fn run(
        &self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
        reconcile: bool,
    ) -> Result<TaskCommandProgress> {
        let task = context
            .durable_task_id()
            .ok_or_else(|| Error::Unauthorized("child preparation has no task".into()))?;
        let host = self.harness.runtime.task_host();
        let record = host.observe_admission(task).await?;
        if record.input != serde_json::to_value(self.request).map_err(storage)?
            || self.request.command.as_ref() != Some(&command)
            || payload != self.request.payload
        {
            return Err(Error::Unauthorized(
                "child preparation differs from its admitted command".into(),
            ));
        }
        let owner = Arc::new(
            host.journal_owner(task, &fence, None)
                .await?
                .with_invocation_cancellation(self.io.cancellation()),
        );
        let preparation = Arc::new(
            owner
                .resource_preparation(self.request.clone(), &mut None)
                .await?,
        );
        let fork = serde_json::from_value(payload).map_err(storage)?;
        if reconcile {
            return Ok(
                if self
                    .preparer
                    .reconcile_for_task(&fork, &preparation, Some(self.io))
                    .await?
                    .is_some()
                {
                    TaskCommandProgress::Ready(Value::Null)
                } else {
                    TaskCommandProgress::Indeterminate
                },
            );
        }
        let (report, _) = self
            .workspace
            .prepare_child(
                self.source,
                self.cut,
                self.preparer,
                &fork,
                &preparation,
                self.io,
            )
            .await?;
        Ok(TaskCommandProgress::Ready(
            serde_json::to_value(crate::Outcome::Succeeded(report)).map_err(storage)?,
        ))
    }
}

impl TaskCommandHost for Commands<'_> {
    fn execute<'a>(
        &'a self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
    ) -> BoxProviderFuture<'a, Result<TaskCommandProgress>> {
        Box::pin(self.run(context, fence, command, payload, false))
    }
    fn reconcile<'a>(
        &'a self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
    ) -> BoxProviderFuture<'a, Result<TaskCommandProgress>> {
        Box::pin(self.run(context, fence, command, payload, true))
    }
}

use super::{
    ExternalAction, ExternalRequest, intake::LIFECYCLE,
    publication::{publish, storage},
    volume,
};
use crate::{
    Error, Result,
    distributed::JournalWrite,
    filesystem::{FilesystemTaskRuntime, TaskCommandHost, TaskCommandProgress, TOOL_TASK_COMMAND_KIND, ToolTaskCommand},
    runtime::TaskContext,
    scheduler::LeaseFence,
    task_volume::{Component, FileKey, FileValue, Query, TaskFile, TaskVolume},
    workflow::WorkflowCommand,
};
use acyclic_fs::{AsyncAuthorityStore, AsyncObjectStore};
use acyclic_stream::{BoxProviderFuture, StreamProvider};
use serde_json::Value;

/// Executes prepared request commands through the runtime's existing executors.
/// Started/result files use the same provider transaction as the task fence.
/// The actor prepares the pinned command; this adapter owns no task state.
pub struct RequestCommands<'a, P, A, O> {
    runtime: &'a FilesystemTaskRuntime<P, A, O>,
    requests: &'a TaskVolume<P>,
    results: &'a TaskVolume<P>,
}

impl<'a, P, A, O> RequestCommands<'a, P, A, O>
where
    P: StreamProvider + 'static,
    A: AsyncAuthorityStore + 'static,
    O: AsyncObjectStore + 'static,
{
    /// Binds caller-writable requests and owner-writable results to one runtime.
    pub fn new(
        runtime: &'a FilesystemTaskRuntime<P, A, O>,
        requests: &'a TaskVolume<P>,
        results: &'a TaskVolume<P>,
    ) -> Self {
        Self {
            runtime,
            requests,
            results,
        }
    }

    fn dispatch(
        &self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
        reconcile: bool,
    ) -> BoxProviderFuture<'_, Result<TaskCommandProgress>> {
        Box::pin(async move {
            let task = context
                .durable_task_id()
                .ok_or_else(|| Error::Unauthorized("request has no admitted task".into()))?;
            if !std::sync::Arc::ptr_eq(&context.harness, self.runtime.harness()) {
                return Err(Error::Unauthorized(
                    "request uses another runtime context".into(),
                ));
            }
            let owner = self
                .runtime
                .task_host()
                .journal_owner(task, &fence, None)
                .await?;
            let provider = owner.stream();
            if !self.requests.uses_provider(&provider) || !self.results.uses_provider(&provider) {
                return Err(Error::Unauthorized(
                    "request volumes use another task provider".into(),
                ));
            }
            owner.verify(reconcile, &mut None).await?;
            let task = Component::new(task.to_string()).map_err(storage)?;
            let operation = Component::new(command.operation_id.to_string()).map_err(storage)?;
            let key = FileKey::Task {
                task: task.clone(),
                file: TaskFile::Request(operation.clone()),
            };
            let mut snapshot = self
                .requests
                .snapshot(&Query::File(key), 1)
                .await
                .map_err(storage)?;
            let original = snapshot
                .files
                .pop()
                .ok_or_else(|| Error::NotFound("task request file is missing".into()))?;
            let request_volume = snapshot.generation.volume;
            drop(snapshot);
            let request = ExternalRequest::from_file(&task, &original).map_err(storage)?;
            drop(original);
            if request.operation_id != command.operation_id {
                return Err(Error::Conflict(
                    "request differs from the retained command".into(),
                ));
            }
            if command.kind == TOOL_TASK_COMMAND_KIND {
                let actual: ToolTaskCommand = serde::Deserialize::deserialize(&payload)
                    .map_err(|error| Error::Invalid(error.to_string()))?;
                if actual != request.tool_call(self.runtime.tool_registry())? {
                    return Err(Error::Conflict("tool command differs from the original native request".into()));
                }
            } else if command.kind != LIFECYCLE {
                return Err(Error::Unsupported("external command kind is not bound".into()));
            }
            let result_key = request.result_key(task.clone()).map_err(storage)?;
            let started = FileValue::new(
                FileKey::Task {
                    task,
                    file: TaskFile::RequestStarted(operation),
                },
                volume::encode(&command).map_err(storage)?,
            );
            let existing = self
                .results
                .snapshot(&Query::File(started.key.clone()), 1)
                .await
                .map_err(storage)?;
            if existing.generation.volume == request_volume {
                return Err(Error::Unauthorized(
                    "request and result volumes must be distinct".into(),
                ));
            }
            if reconcile && existing.files.first() != Some(&started) {
                return Err(Error::Conflict(
                    "request reconciliation has no original dispatch".into(),
                ));
            }
            drop(existing);
            let write = if reconcile {
                JournalWrite::Settlement
            } else {
                JournalWrite::Fresh
            };
            publish(
                self.results,
                &started,
                owner.condition(write, &mut None).await?,
                owner.admission().run_limits.deadline_epoch_ms,
            )
            .await?;
            let cached = self
                .results
                .snapshot(&Query::File(result_key.clone()), 1)
                .await
                .map_err(storage)?;
            if let Some(file) = cached.files.first() {
                return Ok(TaskCommandProgress::Ready(
                    volume::decode(&file.bytes).map_err(storage)?,
                ));
            }
            drop(cached);
            let progress = if command.kind == LIFECYCLE {
                let original: ExternalRequest = serde_json::from_value(payload)
                    .map_err(|error| Error::Invalid(error.to_string()))?;
                if original != request || !matches!(request.action,
                    ExternalAction::Attach {..} | ExternalAction::Detach | ExternalAction::ToolFinished {..}) {
                    return Err(Error::Conflict("lifecycle command differs from its native request".into()));
                }
                if let ExternalAction::ToolFinished {call_id,..}=&request.action {
                    let call = ExternalAction::RunTool {call_id:call_id.clone(),name:String::new(),arguments:Value::Null};
                    let operation=super::normalize::operation(request.host,&request.session_id,&request.caller,&call);
                    let result_key=FileKey::Task {task:Component::new(owner.task_id().to_string()).map_err(storage)?,
                        file:TaskFile::RequestResult(Component::new(operation.to_string()).map_err(storage)?)};
                    if self.results.snapshot(&Query::File(result_key),1).await.map_err(storage)?.files.is_empty() {
                        return Err(Error::Conflict("native completion has no original harness tool result".into()));
                    }
                }
                TaskCommandProgress::Ready(serde_json::json!({
                    "task":owner.task_id(),"operation_id":request.operation_id
                }))
            } else {
                drop(request);
                let commands = self.runtime.commands();
                if reconcile {
                    commands.reconcile(context, fence, command, payload).await?
                } else {
                    commands.execute(context, fence, command, payload).await?
                }
            };
            if let TaskCommandProgress::Ready(value) = &progress {
                let result = FileValue::new(result_key, volume::encode(value).map_err(storage)?);
                publish(
                    self.results,
                    &result,
                    owner.condition(JournalWrite::Settlement, &mut None).await?,
                    None,
                )
                .await?;
            }
            Ok(progress)
        })
    }
}

impl<P, A, O> TaskCommandHost for RequestCommands<'_, P, A, O>
where
    P: StreamProvider + 'static,
    A: AsyncAuthorityStore + 'static,
    O: AsyncObjectStore + 'static,
{
    fn execute<'a>(
        &'a self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
    ) -> BoxProviderFuture<'a, Result<TaskCommandProgress>> {
        self.dispatch(context, fence, command, payload, false)
    }

    fn reconcile<'a>(
        &'a self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
    ) -> BoxProviderFuture<'a, Result<TaskCommandProgress>> {
        self.dispatch(context, fence, command, payload, true)
    }
}

use super::*;
use acyclic_fs::Fs;
use acyclic_harness::task_volume::{Component, Query, TaskVolume};
use acyclic_harness::{
    Admission, AgentId, Capabilities, OperationId, TaskId,
    conversation::{
        ContentResidencyVerifier, Limits, VolumeClass, VolumeOperation, VolumeOwner, VolumeRef,
    },
    core::{AggregateKind, Authority, AuthorityIssuer},
    distributed::{SchedulerPayloadStore, Worker},
    durable_tool::ResumableToolRegistry,
    filesystem::{
        FilesystemContentPublisher, FilesystemContentVerifier, FilesystemHost,
        FilesystemTaskRuntime, TOOL_TASK_COMMAND_KIND, TaskCommandHost, TaskWorkerAttempt,
        TaskWorkerOutcome, ToolTaskCommand,
    },
    resources::ProviderRef,
    runtime::{RuntimeScope, RuntimeTask, TaskRegistry, TaskStateProvider},
    scheduler::{ResourceRequest, SessionLimits},
    workflow::{MachineRegistry, WorkflowCommand},
};
use acyclic_stream::{MemoryStream, StreamPath, SystemUnixMillisClock};
use std::{collections::BTreeMap, sync::Arc};

#[path = "session_runtime/fixture.rs"]
mod fixture;
pub(crate) use fixture::Fixture;

#[tokio::test]
async fn session_executes_the_registered_write_tool_through_the_real_worker() -> Result<()> {
    Box::pin(run_write_session(VolumeBinding::Separate)).await
}

#[tokio::test]
async fn request_volumes_cannot_bypass_the_task_fence_with_another_provider() -> Result<()> {
    Box::pin(run_write_session(VolumeBinding::Foreign)).await
}

#[tokio::test]
async fn request_permissions_cannot_include_owner_result_files() -> Result<()> {
    Box::pin(run_write_session(VolumeBinding::Shared)).await
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum VolumeBinding {
    Separate,
    Foreign,
    Shared,
}

async fn run_write_session(binding: VolumeBinding) -> Result<()> {
    let Fixture {runtime,files,volume,verifier,signed,tasks,revision,publisher,requests,results} = Fixture::new(binding).await?;
    let operation = OperationId::from_bytes([63; 16]);
    let arguments = json!({
        "path":"session.txt","text":"written by the task worker",
        "media_type":"text/plain","display_name":"session.txt"
    });
    let original = request(
        ExternalHost::Pi,
        "tool_call",
        json!({
            "sessionId":"session", "toolCallId":"call", "toolName":"write", "input":arguments
        }),
    )?;
    let tool_operation = original.operation_id;
    let task_folder =
        Component::new(TaskId::from_bytes(operation.into_bytes()).to_string()).map_err(storage)?;
    let file = original.to_file(task_folder.clone()).map_err(storage)?;
    let snapshot = requests
        .snapshot(&Query::File(file.key.clone()), 1)
        .await
        .map_err(storage)?;
    requests
        .append(snapshot.generation, &file)
        .await
        .map_err(storage)?;
    let payload = serde_json::to_vec(&ToolTaskCommand {
        name: "write".into(),
        revision,
        arguments,
    })
    .map_err(|error| invalid(&error))?;
    let payload =
        SchedulerPayloadStore::stage(publisher.as_ref(), tool_operation, "tool", &payload, None)?
            .await?;
    let command = WorkflowCommand {
        operation_id: tool_operation,
        kind: TOOL_TASK_COMMAND_KIND.into(),
        payload,
    };
    let definition = tasks.get::<Value, ()>("acyclic.command_session@1")?;
    let admission = runtime
        .harness()
        .admit(
            operation,
            &definition,
            json!({"command":command,"terminal":true}),
            None,
        )
        .await?;
    let task = match admission {
        Admission::Accepted(RuntimeTask::Durable { task_id, .. }) => task_id,
        _ => return Err(Error::Storage("command session was not admitted".into())),
    };
    assert_eq!(task, TaskId::from_bytes(operation.into_bytes()));
    let worker = Worker {
        id: "session-worker".into(),
        available: ResourceRequest::default(),
        labels: BTreeMap::new(),
    };
    let commands = RequestCommands::new(&runtime, &requests, &results);
    let context = runtime.harness().durable_context(task, operation, None).await?;
    let future = commands.execute(
        context,
        acyclic_harness::scheduler::LeaseFence {
            reservation_id: "unpolled-size-probe".into(),
            placement: worker.id.clone(),
        },
        command,
        Value::Null,
    );
    let bytes = std::mem::size_of_val(&future);
    drop(future);
    eprintln!("external request command future: {bytes} bytes");
    assert!(
        bytes < 16 * 1024,
        "external request command future: {bytes} bytes"
    );
    let attempt = Box::pin(runtime.run_operation(&worker, task, &commands, 8, None)).await?;
    if binding != VolumeBinding::Separate {
        assert!(matches!(
            attempt,
            Some(TaskWorkerAttempt::Progress(
                TaskWorkerOutcome::Reconciling { .. }
            ))
        ));
        let actor_files = results
            .snapshot(&Query::Requests(task_folder), 2)
            .await
            .map_err(storage)?
            .files;
        assert!(
            actor_files
                .iter()
                .all(|file| file.key.owner() == acyclic_harness::task_volume::Owner::Caller)
        );
        let reader = FilesystemContentVerifier::new(files, &verifier, &signed, 65_536, None)?;
        assert!(
            reader
                .read_private_path(&volume, "", "session.txt", None, 65_536, None)?
                .await
                .is_err()
        );
        return Ok(());
    }
    match attempt {
        Some(TaskWorkerAttempt::Progress(TaskWorkerOutcome::Completed { task: completed })) => {
            assert_eq!(completed, task)
        }
        Some(TaskWorkerAttempt::Unresolved { error, .. }) => return Err(error),
        Some(TaskWorkerAttempt::Progress(TaskWorkerOutcome::Suspended { revision, .. })) => {
            return Err(Error::Storage(format!(
                "command session suspended at {revision}"
            )));
        }
        Some(TaskWorkerAttempt::Progress(TaskWorkerOutcome::Reconciling { .. })) => {
            return Err(Error::Storage(
                "command session requires reconciliation".into(),
            ));
        }
        Some(TaskWorkerAttempt::Progress(TaskWorkerOutcome::Yielded { .. })) => {
            return Err(Error::Storage(
                "command session exhausted its transition allowance".into(),
            ));
        }
        None => return Err(Error::Storage("command session was not runnable".into())),
    }
    assert_eq!(
        runtime.task_host().outcome(task).await?,
        Some(acyclic_harness::Outcome::Succeeded(Value::Null))
    );
    let reader = FilesystemContentVerifier::new(files, &verifier, &signed, 65_536, None)?;
    let (file, content, _) = reader
        .read_private_path(&volume, "", "session.txt", None, 65_536, None)?
        .await?;
    assert_eq!(file.path(), "session.txt");
    assert_eq!(&*content, b"written by the task worker");
    let result = results
        .snapshot(
            &Query::File(original.result_key(task_folder).map_err(storage)?),
            1,
        )
        .await
        .map_err(storage)?;
    assert_eq!(result.files.len(), 1);
    let outcome: acyclic_harness::Outcome<acyclic_harness::tool::files::FileResult> =
        serde_json::from_slice(&result.files[0].bytes).map_err(|error| invalid(&error))?;
    let acyclic_harness::Outcome::Succeeded(written) = outcome else {
        return Err(Error::Storage("write command did not succeed".into()));
    };
    assert_eq!(written.file.volume(), &volume);
    assert_eq!(written.file.path(), "session.txt");
    let answer = original
        .send(
            &requests,
            &results,
            Component::new(task.to_string()).map_err(storage)?,
        )
        .await
        .map_err(storage)?;
    assert_eq!(
        answer,
        serde_json::from_slice::<Value>(&result.files[0].bytes).map_err(|error| invalid(&error))?
    );
    // Completed work cannot re-execute its publication on a client retry.
    assert!(
        runtime
            .run_operation(&worker, task, &commands, 8, None)
            .await?
            .is_none()
    );
    Ok(())
}

fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

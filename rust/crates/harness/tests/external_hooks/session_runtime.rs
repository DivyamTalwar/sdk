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
enum VolumeBinding {
    Separate,
    Foreign,
    Shared,
}

async fn run_write_session(binding: VolumeBinding) -> Result<()> {
    let provider = ProviderRef::new(
        "session-worker".into(),
        "filesystem".into(),
        "2".into(),
        &mut None,
    )?;
    let files = FilesystemHost::new(Fs::memory(), &provider, None)?;
    let agent = AgentId::from_bytes([61; 16]);
    let volume = VolumeRef::new(
        provider,
        "private".into(),
        VolumeClass::AgentPrivate,
        VolumeOwner::Agent(agent),
        &mut None,
    )?;
    files.create_volume(&volume).await?;
    let issuer = AuthorityIssuer::new(
        "session-worker",
        [62; 32],
        Authority {
            kind: AggregateKind::Task,
            id: "session-worker".into(),
        },
    );
    let grants = Capabilities::new([
        "operation:declare".to_owned(),
        "operation:observe".to_owned(),
        "operation:cancel".to_owned(),
        "task:spawn:acyclic.command_session@1".to_owned(),
        "tool:call:write".to_owned(),
        volume.capability(VolumeOperation::Read, &mut None)?,
        volume.capability(VolumeOperation::Write, &mut None)?,
        volume.directory_read_capability("", &mut None)?,
    ]);
    let signed = issuer.issue("owner", &grants, Some(agent), None, &mut None)?;
    let verifier = issuer.verifier(&mut None)?;
    let scope = RuntimeScope::new(grants, Limits::default())?;
    let machine = Arc::new(CommandSessionMachine::new());
    let mut tasks = TaskRegistry::default();
    tasks.register(machine.definition()?)?;
    let mut machines = MachineRegistry::default();
    machines.register(machine, &mut None)?;
    let mut tools = acyclic_harness::tool::ToolRegistry::default();
    let write = acyclic_harness::tool::files::write()?;
    let revision = write.definition.revision.clone();
    tools.register((write, None), &mut None, None)?;
    let stream = Arc::new(MemoryStream::default());
    let requests = TaskVolume::open(
        stream.clone(),
        StreamPath::new("requests").map_err(storage)?,
    )
    .await
    .map_err(storage)?;
    let results = TaskVolume::open(
        if binding == VolumeBinding::Foreign {
            Arc::new(MemoryStream::default())
        } else {
            stream.clone()
        },
        StreamPath::new(if binding == VolumeBinding::Shared {
            "requests"
        } else {
            "results"
        })
        .map_err(storage)?,
    )
    .await
    .map_err(storage)?;
    let runtime = Box::pin(FilesystemTaskRuntime::open(
        stream,
        files.clone(),
        &volume,
        &verifier,
        &signed,
        &scope,
        &tasks,
        &machines,
        &tools,
        SessionLimits {
            active_tasks: 1,
            total_tasks: 1,
            depth: 1,
            model_steps: 1,
        },
        1,
        65_536,
        Arc::new(SystemUnixMillisClock),
        None,
        &[],
        &ResumableToolRegistry::default(),
        None,
    ))
    .await?
    .with_output_volume(&volume, None)?;
    let publisher =
        FilesystemContentPublisher::new(files.clone(), &volume, &verifier, &signed, 65_536, None)?;
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
    let arguments: Option<(
        acyclic_harness::runtime::TaskContext,
        acyclic_harness::scheduler::LeaseFence,
        WorkflowCommand,
    )> = None;
    fn future_size<F: Future>(_: impl FnOnce() -> F) -> usize {
        std::mem::size_of::<F>()
    }
    let bytes = future_size(|| {
        let (context, fence, command) = arguments.unwrap();
        commands.execute(context, fence, command, Value::Null)
    });
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

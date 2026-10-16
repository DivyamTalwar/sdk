use super::*;
use crate::{
    filesystem::{TaskCommandHost, TaskCommandProgress},
    runtime::TaskContext,
    scheduler::LeaseFence,
    workflow::WorkflowCommand,
};
use acyclic_stream::BoxProviderFuture;

struct Published<'a> {
    commands: RequestCommands<'a, LocalStream, LocalAuthorityBackend, LocalObjectBackend>,
    reached: &'a tokio::sync::Notify,
}

impl TaskCommandHost for Published<'_> {
    fn execute<'a>(
        &'a self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
    ) -> BoxProviderFuture<'a, Result<TaskCommandProgress>> {
        Box::pin(async move {
            self.commands
                .execute(context, fence, command, payload)
                .await?;
            self.reached.notify_one();
            std::future::pending().await
        })
    }

    fn reconcile<'a>(
        &'a self,
        context: TaskContext,
        fence: LeaseFence,
        command: WorkflowCommand,
        payload: Value,
    ) -> BoxProviderFuture<'a, Result<TaskCommandProgress>> {
        self.commands.reconcile(context, fence, command, payload)
    }
}

#[tokio::test]
async fn parallel_native_calls_wait_and_both_publish_their_results() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let harness = LocalHarness::open(LocalHarnessConfig::external(directory.path())).await?;
    let host = crate::external_executor::ExternalHost::Pi;
    let attach = crate::external_executor::request(
        host,
        "session_start",
        serde_json::json!({"sessionId":"parallel", "cwd":directory.path()}),
    )?;
    harness.external_request(&attach).await?;
    let tool = |name: &str| {
        crate::external_executor::request(
            host,
            "tool_call",
            serde_json::json!({"sessionId":"parallel", "toolCallId":name, "toolName":"write",
            "input":{"path":name, "content":name}}),
        )
    };
    let first = tool("first.txt")?;
    let second = tool("second.txt")?;
    let (left, right) = tokio::join!(
        harness.external_request(&first),
        harness.external_request(&second)
    );
    crate::external_executor::tool_result(left?)?;
    crate::external_executor::tool_result(right?)?;
    for name in ["first.txt", "second.txt"] {
        assert_eq!(
            std::fs::read_to_string(directory.path().join(name)).map_err(storage)?,
            name
        );
    }
    Ok(())
}

#[tokio::test]
async fn published_result_recovers_before_retry_or_the_next_request() -> Result<()> {
    for retry in [false, true] {
        let directory = tempfile::tempdir().map_err(storage)?;
        let native = |call: &str, text: &str| {
            crate::external_executor::request(
                crate::external_executor::ExternalHost::Pi,
                "tool_call",
                serde_json::json!({"sessionId":"recovery", "toolCallId":call,
                "toolName":"write", "input":{"path":"note.txt", "content":text}}),
            )
        };
        let attach = crate::external_executor::request(
            crate::external_executor::ExternalHost::Pi,
            "session_start",
            serde_json::json!({"sessionId":"recovery", "cwd":directory.path()}),
        )?;
        let first = native("first", "first")?;
        let second = native("second", "second")?;
        let harness = LocalHarness::open(LocalHarnessConfig::external(directory.path())).await?;
        harness.external_request(&attach).await?;
        let task = Component::new(first.task_id().to_string()).map_err(storage)?;
        let (requests, results) = harness.external_volumes().await?;
        let file = first.to_file(task.clone()).map_err(storage)?;
        let cut = requests
            .snapshot(&Query::File(file.key.clone()), 1)
            .await
            .map_err(storage)?;
        requests
            .append(cut.generation, &file)
            .await
            .map_err(storage)?;
        let publisher = FilesystemContentPublisher::new(
            harness.files.clone(),
            &harness.journal,
            &harness.verifier,
            &harness.signed,
            harness.limits.file_bytes,
            None,
        )?;
        RequestConsumer::new(
            &harness.runtime,
            &requests,
            &results,
            publisher.as_ref(),
            "",
        )
        .accept(&task, &file)
        .await?;
        let reached = tokio::sync::Notify::new();
        let interrupted = Published {
            commands: RequestCommands::new(&harness.runtime, &requests, &results),
            reached: &reached,
        };
        let mut drive = Box::pin(harness.advance(first.task_id(), &interrupted));
        tokio::select! {
            result = &mut drive => panic!("worker finished before the cancellation cut: {result:?}"),
            _ = reached.notified() => {}
        }
        drop(drive);
        assert!(
            harness
                .runtime
                .task_host()
                .workflow_slot(first.task_id(), None, &mut None)
                .await?
                .is_some_and(
                    |(phase, slot)| phase == crate::scheduler::OperationPhase::Running
                        && slot.input.is_some()
                )
        );
        let answer = first.follow_result(&results, task).await.map_err(storage)?;
        assert_eq!(
            std::fs::read_to_string(directory.path().join("note.txt")).map_err(storage)?,
            "first"
        );
        drop(interrupted);
        drop(publisher);
        drop(requests);
        drop(results);
        drop(harness);
        let reopened = LocalHarness::open(LocalHarnessConfig::external(directory.path())).await?;
        if retry {
            assert_eq!(reopened.external_request(&first).await?, answer);
            let (_, slot) = reopened
                .runtime
                .task_host()
                .workflow_slot(first.task_id(), None, &mut None)
                .await?
                .ok_or_else(|| Error::Conflict("missing settled slot".into()))?;
            assert!(slot.waiting_command.is_none());
            assert!(slot.input.is_none());
        }
        crate::external_executor::tool_result(reopened.external_request(&second).await?)?;
        assert_eq!(
            std::fs::read_to_string(directory.path().join("note.txt")).map_err(storage)?,
            "second"
        );
    }
    Ok(())
}

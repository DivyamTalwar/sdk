//! Separate native hook processes progress while their parent awaits a child.
#[path = "support/native_hooks.rs"]
mod native_hooks;
use acyclic_harness::{
    external_executor::{ExternalHost, ExternalRequest, request},
    task_volume::{Component, FileKey, Query, TaskFile, TaskVolume},
};
use acyclic_stream::grpc::Client;
use futures::StreamExt as _;
use native_hooks::*;
use serde_json::json;

async fn started(files: &TaskVolume<Client>, call: &ExternalRequest) -> Result {
    let key = FileKey::Task {
        task: Component::new(call.task_id().to_string())?,
        file: TaskFile::RequestStarted(Component::new(call.operation_id.to_string())?),
    };
    let mut observed = files.observe(Query::File(key.clone()), None, 1).await?;
    if observed
        .snapshot
        .is_some_and(|snapshot| snapshot.files.iter().any(|file| file.key == key))
    {
        return Ok(());
    }
    while let Some(change) = observed.changes.next().await {
        let change = change?;
        if change.file.as_ref().is_some_and(|file| file.key == key) {
            return Ok(());
        }
    }
    Err("command-start follow ended".into())
}

#[tokio::test]
async fn pending_native_parent_joins_do_not_block_child_hooks_for_any_host() -> Result {
    let fixture = Fixture::new()?;
    let cwd = fixture.path();
    for host in [
        ExternalHost::Codex,
        ExternalHost::ClaudeCode,
        ExternalHost::Pi,
    ] {
        let pi = host == ExternalHost::Pi;
        let session = host.name();
        let attach = if pi { "session_start" } else { "SessionStart" };
        hook(
            cwd,
            host.name(),
            attach,
            json!({"sessionId":session,"cwd":cwd}),
        )
        .await?;
        hook(
            cwd,
            host.name(),
            if pi {
                "subagent_start"
            } else {
                "SubagentStart"
            },
            json!({"sessionId":session,"agentId":"child","agentType":"worker"}),
        )
        .await?;
        hook(
            cwd,
            host.name(),
            attach,
            json!({"sessionId":session,"agentId":"child","cwd":cwd}),
        )
        .await?;
        let event = if pi { "subagent_stop" } else { "SubagentStop" };
        let input = json!({"sessionId":session,"agentId":"child"});
        let join = request(host, event, input.clone())?;
        let results = files(cwd, &format!("external/results/{}", join.task_id())).await?;
        let mut waiting = Box::pin(hook(cwd, host.name(), event, input));
        tokio::select! {
            result = &mut waiting => panic!("join completed before child: {result:?}"),
            result = started(&results,&join) => result?,
        }
        assert!(
            results
                .snapshot(
                    &Query::File(join.result_key(Component::new(join.task_id().to_string())?)?),
                    1
                )
                .await?
                .files
                .is_empty()
        );
        // Both subprocesses must progress through the same surviving owner.
        let (joined, ended) = tokio::join!(
            waiting,
            hook(
                cwd,
                host.name(),
                if pi { "session_shutdown" } else { "SessionEnd" },
                json!({"sessionId":session,"agentId":"child"})
            )
        );
        assert_eq!(joined?, json!({"Succeeded":null}));
        ended?;
    }
    fixture.close().await
}

#[tokio::test]
async fn cancelling_a_passive_native_tool_settles_it_and_keeps_the_session_usable() -> Result {
    let fixture = Fixture::new()?;
    let cwd = fixture.path();
    hook(
        cwd,
        "pi",
        "session_start",
        json!({"sessionId":"cancel-wait","cwd":cwd}),
    )
    .await?;
    let input = json!({"sessionId":"cancel-wait","toolCallId":"receive",
        "toolName":"acyclic_receive","input":{"after":0}});
    let call = request(ExternalHost::Pi, "tool_call", input.clone())?;
    let results = files(cwd, &format!("external/results/{}", call.task_id())).await?;
    let waiting = start_hook(cwd, "pi", "tool_call", input).await?;
    started(&results, &call).await?;
    assert!(
        results
            .snapshot(
                &Query::File(call.result_key(Component::new(call.task_id().to_string())?)?),
                1
            )
            .await?
            .files
            .is_empty()
    );
    // A cancellation is published independently of the occupied command slot.
    let original = start_hook(
        cwd,
        "pi",
        "tool_cancel",
        json!({"sessionId":"cancel-wait","toolCallId":"receive"}),
    )
    .await?;
    // Pi's raw tool-call process reports a failed tool as a nonzero exit. Use
    // the request result file to assert settlement without treating it as success.
    let settled = async {
        let key = call.result_key(Component::new(call.task_id().to_string())?)?;
        let mut observed = results.observe(Query::File(key), None, 1).await?;
        if let Some(snapshot) = observed.snapshot {
            if let Some(file) = snapshot.files.first() {
                return Ok::<_, Box<dyn std::error::Error>>(serde_json::from_slice::<
                    serde_json::Value,
                >(&file.bytes)?);
            }
        }
        while let Some(change) = observed.changes.next().await {
            if let Some(file) = change?.file {
                return Ok(serde_json::from_slice(&file.bytes)?);
            }
        }
        Err("result follow ended".into())
    };
    let answer = settled.await?;
    assert!(answer.get("Failed").is_some(), "{answer}");
    let cancelled = original.wait_with_output().await?;
    assert!(
        cancelled.status.success(),
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    assert!(!waiting.wait_with_output().await?.status.success());
    hook(
        cwd,
        "pi",
        "tool_call",
        json!({"sessionId":"cancel-wait","toolCallId":"next",
        "toolName":"write","input":{"path":"after-cancel.txt","content":"usable"}}),
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(cwd.join("after-cancel.txt"))?,
        "usable"
    );
    fixture.close().await
}

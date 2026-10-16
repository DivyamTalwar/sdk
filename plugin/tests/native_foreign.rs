//! Native hosts execute additive tools; hooks retain authenticated history.
#[path = "support/native_hooks.rs"]
mod native_hooks;
use acyclic_harness::{
    external_executor::{ExternalAction, ExternalHost, ExternalRequest, request},
    task_volume::{Component, Query, TaskVolume},
};
use acyclic_stream::{StreamPath, grpc::Client};
use native_hooks::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::AsyncWriteExt as _;

async fn request_files(fixture: &Fixture) -> Result<TaskVolume<Client>> {
    let root = fixture.path().parent().unwrap().join("owner");
    let address: Value = serde_json::from_slice(&std::fs::read(root.join("address.json"))?)?;
    let secret: [u8; 32] = std::fs::read(root.join("credential"))?
        .try_into()
        .map_err(|_| "invalid owner key")?;
    let path = std::fs::canonicalize(fixture.path())?;
    let scope = blake3::hash(path.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string();
    let token = format!(
        "{scope}.{}",
        blake3::keyed_hash(&secret, scope.as_bytes()).to_hex()
    );
    let client = Client::connect_eager_with_ca_certificate(
        address["endpoint"].as_str().ok_or("missing endpoint")?,
        &token,
        address["certificate"]
            .as_str()
            .ok_or("missing certificate")?
            .as_bytes(),
    )
    .await?;
    Ok(TaskVolume::open_existing(Arc::new(client), StreamPath::new("external/requests")?).await?)
}

async fn recorded(files: &TaskVolume<Client>, expected: &ExternalRequest) -> Result {
    let task = Component::new(expected.task_id().to_string())?;
    let file = expected.to_file(task.clone())?;
    let snapshot = files.snapshot(&Query::File(file.key.clone()), 1).await?;
    assert_eq!(snapshot.files, [file]);
    assert_eq!(
        ExternalRequest::from_file(&task, &snapshot.files[0])?,
        *expected
    );
    Ok(())
}

#[tokio::test]
async fn foreign_mcp_and_native_calls_are_host_executed_and_recorded_for_every_host() -> Result {
    let fixture = Fixture::new()?;
    let cwd = fixture.path();
    for host in [
        ExternalHost::Codex,
        ExternalHost::ClaudeCode,
        ExternalHost::Pi,
    ] {
        let pi = host == ExternalHost::Pi;
        let session = uuid::Uuid::new_v4().to_string();
        hook(
            cwd,
            host.name(),
            if pi { "session_start" } else { "SessionStart" },
            json!({"session_id":session,"cwd":cwd}),
        )
        .await?;
        let files = request_files(&fixture).await?;
        for name in [
            "mcp__configured__write",
            if pi { "custom_write" } else { "Write" },
        ] {
            let filename = format!("{}-{name}.txt", host.name());
            let event = if pi { "tool_call" } else { "PreToolUse" };
            let input = json!({"session_id":session,"cwd":cwd,"tool_use_id":name,"tool_name":name,
                "tool_input":{"path":filename,"content":"host result"},"hostExecuted":true});
            let expected = request(host, event, input.clone())?;
            assert!(matches!(expected.action, ExternalAction::HostTool { .. }));
            let answer = hook(cwd, host.name(), event, input).await?;
            if pi {
                assert_eq!(answer, json!({"block":false}));
            } else {
                assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");
                assert!(answer["hookSpecificOutput"].get("updatedInput").is_none());
            }
            assert!(
                !cwd.join(&filename).exists(),
                "harness executed the native effect"
            );
            recorded(&files, &expected).await?;
            // This is the host's effect, between its permission and completion hooks.
            std::fs::write(cwd.join(&filename), "host result")?;
            let completion = json!({"session_id":session,"cwd":cwd,"tool_use_id":name,
                "tool_response":{"path":filename},"content":[{"type":"text","text":"host result"}],
                "details":{"path":filename},"isError":false});
            let event = if pi { "tool_result" } else { "PostToolUse" };
            let expected = request(host, event, completion.clone())?;
            hook(cwd, host.name(), event, completion).await?;
            recorded(&files, &expected).await?;
            assert_eq!(std::fs::read_to_string(cwd.join(filename))?, "host result");
        }
        // A completion without an authenticated original call still fails.
        let mut child = command(cwd)
            .args([
                "__hook",
                host.name(),
                if pi { "tool_result" } else { "PostToolUse" },
            ])
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("missing stdin")?
            .write_all(&serde_json::to_vec(&json!({
                "session_id":session,"cwd":cwd,"tool_use_id":"unobserved",
                "tool_response":{},"content":[],"isError":false,
            }))?)
            .await?;
        let rejected = child.wait_with_output().await?;
        assert!(!rejected.status.success());
        drop(files);
        hook(
            cwd,
            host.name(),
            if pi { "session_shutdown" } else { "SessionEnd" },
            json!({"session_id":session,"cwd":cwd}),
        )
        .await?;
    }
    fixture.close().await?;
    Ok(())
}

//! Native hook/MCP processes share LocalHarness request and result files.
use serde_json::{Value, json};
use std::path::Path;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    process::Child,
};

#[path = "support/native_hooks.rs"]
mod native_hooks;
use native_hooks::*;

#[path = "native_tools/commands.rs"]
mod commands;

#[path = "native_tools/parallel.rs"]
mod parallel;

async fn result(directory: &Path, arguments: Value) -> Result<Value> {
    let mut child: Child = command(directory).arg("__tools").spawn()?;
    let mut input = child.stdin.take().ok_or("MCP stdin missing")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("MCP stdout missing")?).lines();
    let write = |value: Value| format!("{value}\n").into_bytes();
    input.write_all(&write(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"native-probe","version":"1"}}}))).await?;
    let initialized: Value = serde_json::from_str(
        &output
            .next_line()
            .await?
            .ok_or("MCP initialization ended")?,
    )?;
    assert_eq!(initialized["id"], 1);
    assert!(initialized.get("error").is_none(), "{initialized}");
    input
        .write_all(&write(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        ))
        .await?;
    input.write_all(&write(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write","arguments":arguments}}))).await?;
    let response: Value =
        serde_json::from_str(&output.next_line().await?.ok_or("MCP result ended")?)?;
    assert_eq!(response["id"], 2);
    assert!(response.get("error").is_none(), "{response}");
    drop(input);
    assert!(child.wait().await?.success());
    Ok(response["result"].clone())
}

#[tokio::test]
async fn native_hooks_publish_once_and_mcp_only_returns_the_stored_result() -> Result {
    tokio::time::timeout(std::time::Duration::from_secs(8),async {
        for host in ["codex","claude-code","pi"] {
            let directory = Fixture::new()?;
            let session = uuid::Uuid::new_v4().to_string();
            let cwd = directory.path().to_str().ok_or("non-UTF8 test directory")?;
            let pi = host == "pi";
            hook(directory.path(),host,if pi {"session_start"} else {"SessionStart"},json!({"session_id":session,"cwd":cwd})).await?;
            let arguments = json!({"path":"native.txt","text":"one harness write","media_type":"text/plain","display_name":"native.txt"});
            let request = json!({"session_id":session,"cwd":cwd,"tool_use_id":"call","tool_name":if pi {"write"} else {"mcp__acyclic__write"},"tool_input":arguments});
            let answer = hook(directory.path(),host,if pi {"tool_call"} else {"PreToolUse"},request.clone()).await?;
            assert_eq!(std::fs::read_to_string(directory.path().join("native.txt"))?,"one harness write");
            let published = std::fs::metadata(directory.path().join("native.txt"))?.modified()?;
            if pi {
                assert_eq!(answer["isError"],false);
                assert!(answer["details"]["file"].is_object());
            } else {
                assert_eq!(answer["hookSpecificOutput"]["permissionDecision"],"allow");
                let arguments = answer["hookSpecificOutput"]["updatedInput"].clone();
                assert!(arguments["_acyclic"].is_string());
                let first = result(directory.path(),arguments.clone()).await?;
                assert!(first["structuredContent"]["file"].is_object(),"{first}");
                assert_eq!(result(directory.path(),arguments.clone()).await?,first);
                let mut changed = arguments;
                changed["text"] = json!("forged");
                assert_eq!(result(directory.path(),changed).await?["isError"],true);
            }
            assert_eq!(hook(directory.path(),host,if pi {"tool_call"} else {"PreToolUse"},request).await?,answer);
            assert_eq!(std::fs::metadata(directory.path().join("native.txt"))?.modified()?,published);
            hook(directory.path(),host,if pi {"session_shutdown"} else {"SessionEnd"},json!({"session_id":session,"cwd":cwd})).await?;
            directory.close().await?;
        }
        Ok::<_,Box<dyn std::error::Error>>(())
    }).await??;
    Ok(())
}

use super::*;
use crate::{
    mcp::{McpCatalog, McpDiscoveryPolicy, McpSchemaExposure},
    tool::{JsonToolProjection, ToolInvocation, ToolProjection, ToolRegistry},
};
use bytes::BytesMut;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio_util::codec::{Decoder as _, Encoder as _};

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Invalid(error.to_string())
}

async fn fixture(
    mode: &'static str,
) -> Result<(
    StdioMcpTransport,
    tokio::task::JoinHandle<Result<Vec<String>>>,
)> {
    let (client, server) = tokio::io::duplex(1024);
    let (read, mut write) = tokio::io::split(server);
    let server = tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(read).lines();
        let mut methods = Vec::new();
        let mut calls = 0;
        let mut pending = None;
        while let Some(line) = lines.next_line().await.map_err(Error::process_io)? {
            let mut request: Value = serde_json::from_str(&line).map_err(invalid)?;
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("fixture method missing"))?
                .to_owned();
            let id = request.get_mut("id").map(Value::take);
            methods.push(method.clone());
            let Some(id) = id else {
                continue;
            };
            let result = match method.as_str() {
                "initialize" => json!({
                    "protocolVersion":"2025-11-25", "capabilities":{"tools":{}},
                    "serverInfo":{"name":"fixture", "version":"1"}
                }),
                "tools/list" if mode == "reverse" => {
                    json!({"tools":[],"nextCursor":request["params"]["cursor"]})
                }
                "tools/list" => json!({"tools":[{"name":"echo", "description":"Count calls",
                    "inputSchema":{"type":"object"}}]}),
                "tools/call" => {
                    calls += 1;
                    json!({"content":[{"type":"text","text":calls.to_string()}]})
                }
                _ => return Err(invalid("unexpected fixture method")),
            };
            let bytes = if method == "initialize" || matches!(mode, "success" | "reverse") {
                serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":id, "result":result}))
                    .map_err(invalid)?
            } else {
                match mode {
                    "oversize" => serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,
                        "result":{"tools":[{"name":"large","description":"x".repeat(8192),
                        "inputSchema":{"type":"object"}}]}}))
                    .map_err(invalid)?,
                    "malformed" => b"not json".to_vec(),
                    "wrong-id" => {
                        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":999,"result":result}))
                            .map_err(invalid)?
                    }
                    "timeout" => continue,
                    "closed" => break,
                    _ => return Err(invalid("unknown fixture mode")),
                }
            };
            let first = if mode == "reverse" && method != "initialize" {
                if pending.is_none() {
                    pending = Some(bytes);
                    continue;
                }
                pending.take()
            } else {
                None
            };
            for bytes in std::iter::once(bytes).chain(first) {
                if let Err(error) = write.write_all(&bytes).await {
                    if mode == "oversize" {
                        return Ok(methods);
                    }
                    return Err(Error::process_io(error));
                }
                write.write_all(b"\n").await.map_err(Error::process_io)?;
            }
            write.flush().await.map_err(Error::process_io)?;
        }
        Ok(methods)
    });
    let (read, write) = tokio::io::split(client);
    let client =
        StdioMcpTransport::bind(read, write, 4096, Duration::from_millis(50), None).await?;
    Ok((client, server))
}

#[tokio::test(start_paused = true)]
async fn concurrent_requests_receive_their_own_out_of_order_responses() -> Result<()> {
    let (transport, server) = fixture("reverse").await?;
    let (first, second) = tokio::join!(
        transport.rpc(
            OperationId::from_bytes([5; 16]),
            "tools/list",
            json!({"cursor":"first"}),
            None
        ),
        transport.rpc(
            OperationId::from_bytes([6; 16]),
            "tools/list",
            json!({"cursor":"second"}),
            None
        ),
    );
    assert_eq!(first?["nextCursor"], "first");
    assert_eq!(second?["nextCursor"], "second");
    drop(transport);
    let methods = server.await.map_err(invalid)??;
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list"
        ]
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn one_session_discovers_installs_calls_and_reconciles_without_replay() -> Result<()> {
    let (transport, server) = fixture("success").await?;
    let transport = Arc::new(transport);
    let listed = transport
        .rpc(
            OperationId::from_bytes([1; 16]),
            "tools/list",
            json!({}),
            None,
        )
        .await?;
    let catalog = McpCatalog {
        server: "configured".into(),
        revision: "1".into(),
        schema_exposure: McpSchemaExposure::Eager,
        discovery: McpDiscoveryPolicy::Disabled,
        projection_schema: crate::tool::json_projection_schema(json!({})),
        tools: serde_json::from_value(
            listed
                .get("tools")
                .cloned()
                .ok_or_else(|| invalid("tools missing"))?,
        )
        .map_err(invalid)?,
    };
    let remote: Arc<dyn McpToolTransport> = transport.clone();
    let projection: Arc<dyn ToolProjection> = Arc::new(JsonToolProjection);
    let mut registry = ToolRegistry::default();
    catalog.install(&mut registry, None, &remote, &projection, 8, 16_384)?;
    assert_eq!(registry.definitions(None)?[0].name, "mcp.configured.echo");
    let tool = registry
        .get("mcp.configured.echo", None, None)?
        .ok_or_else(|| invalid("installed tool missing"))?;
    let invocation = ToolInvocation::for_model_call(
        OperationId::from_bytes([2; 16]),
        0,
        "call".into(),
        "mcp.configured.echo".into(),
        json!({}),
        None,
    )?;
    let result = tool.executor.execute(invocation, None).await?;
    assert_eq!(result.value["content"][0]["text"], "1");
    let operation = OperationId::from_bytes([3; 16]);
    let result = transport.call(operation, "echo", json!({}), None).await?;
    assert_eq!(result["content"][0]["text"], "2");
    assert!(transport.reconcile(operation, None).await?.is_none());
    assert!(matches!(
        transport
            .rpc(
                operation,
                "tools/call",
                json!({"name":"echo","task":{}}),
                None
            )
            .await,
        Err(Error::Unsupported(_))
    ));
    drop(registry);
    drop(remote);
    drop(transport);
    let methods = server.await.map_err(invalid)??;
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call",
            "tools/call"
        ]
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn lost_invalid_oversize_and_late_responses_never_redispatch() -> Result<()> {
    for mode in ["closed", "malformed", "oversize", "wrong-id", "timeout"] {
        let (transport, server) = fixture(mode).await?;
        let operation = OperationId::from_bytes([9; 16]);
        assert!(
            matches!(transport.rpc(operation, "tools/list", json!({}), None).await,
            Err(Error::Indeterminate(id)) if id == operation),
            "{mode}"
        );
        assert!(transport.reconcile(operation, None).await?.is_none());
        drop(transport);
        let methods = server.await.map_err(invalid)??;
        assert_eq!(
            methods
                .iter()
                .filter(|method| method.as_str() == "tools/list")
                .count(),
            1,
            "{mode}"
        );
    }
    Ok(())
}

#[test]
fn frame_limits_apply_both_directions_without_retaining_a_partial_request() -> Result<()> {
    let mut codec = BoundedCodec::<Value>::new(16);
    let mut bytes = BytesMut::from(b"prior\n".as_slice());
    assert!(codec.encode(json!("x".repeat(32)), &mut bytes).is_err());
    assert_eq!(bytes.as_ref(), b"prior\n");
    bytes.clear();
    codec
        .encode(json!("x".repeat(14)), &mut bytes)
        .map_err(Error::process_io)?;
    assert_eq!(bytes.len(), 17);
    assert_eq!(
        codec.decode(&mut bytes).map_err(Error::process_io)?,
        Some(json!("x".repeat(14)))
    );
    bytes.extend_from_slice(&[b'x'; 17]);
    assert!(codec.decode(&mut bytes).is_err());
    Ok(())
}

#[tokio::test]
async fn native_child_is_initialized_once_and_kept_until_the_transport_drops() -> Result<()> {
    let directory = tempfile::tempdir().map_err(Error::process_io)?;
    let initialize = r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}"#;
    let tools = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
    #[cfg(windows)]
    let (filename, script) = (
        "server.cmd",
        format!(
            "@echo off\nset /p input=\necho {initialize}\nset /p input=\nset /p input=\necho {tools}\nset /p input=\n"
        ),
    );
    #[cfg(not(windows))]
    let (filename, script) = (
        "server.sh",
        format!(
            "IFS= read -r input\nprintf '%s\\n' '{initialize}'\nIFS= read -r input\nIFS= read -r input\nprintf '%s\\n' '{tools}'\nIFS= read -r input\n"
        ),
    );
    let path = directory.path().join(filename);
    std::fs::write(&path, script).map_err(Error::process_io)?;
    #[cfg(windows)]
    let command =
        acyclic_native_runtime::shell_command(&format!("call \"{}\"", path.display()), Some("cmd"));
    #[cfg(not(windows))]
    let command =
        acyclic_native_runtime::shell_command(&format!(". \"{}\"", path.display()), Some("sh"));
    let transport = StdioMcpTransport::spawn(command, 4096, 5_000).await?;
    let tools = transport
        .rpc(
            OperationId::from_bytes([4; 16]),
            "tools/list",
            json!({}),
            None,
        )
        .await?;
    assert_eq!(tools["tools"], json!([]));
    assert!(!transport.service.is_closed());
    drop(transport);
    Ok(())
}

#[tokio::test]
async fn invalid_bounds_and_spawn_failures_are_explicit() -> Result<()> {
    for (bytes, timeout) in [(0, 1), (1, 0)] {
        assert!(matches!(
            StdioMcpTransport::spawn(Command::new("not-used"), bytes, timeout).await,
            Err(Error::Invalid(_))
        ));
    }
    let directory = tempfile::tempdir().map_err(Error::process_io)?;
    let command = Command::new(directory.path().join("nonexistent-mcp-server"));
    assert!(matches!(
        StdioMcpTransport::spawn(command, 4096, 5_000).await,
        Err(Error::Process {
            kind: "not_found",
            ..
        })
    ));
    Ok(())
}

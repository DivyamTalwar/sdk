//! Real argv/provider/LocalHarness/client qualification over a loopback endpoint.
use acyclic_harness::{
    OperationId,
    client::prepare_local_prompt,
    task_volume::{Change, FileKey, TaskFile},
};
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

const ANSWER: &str = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"fixture answer\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":64,\"output_tokens\":2}}}\n\n";

async fn execute(
    workspace: &Path,
    catalog: &Path,
    settings: &Path,
    operation: OperationId,
    jsonl: bool,
) -> Vec<u8> {
    let mut input = tempfile::NamedTempFile::new().unwrap();
    if jsonl {
        writeln!(
            input,
            "{}",
            serde_json::to_string(&prepare_local_prompt(operation, "hello").unwrap()).unwrap()
        )
        .unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_graphcoder"));
    command
        .current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .arg("--catalog")
        .arg(catalog)
        .arg("--settings-root")
        .arg(settings)
        .args([
            "--model",
            "graphcoder-qual/tiny",
            "--operation",
            &operation.to_string(),
        ])
        .stdin(Stdio::from(input.reopen().unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if jsonl {
        command.args(["--input-format", "jsonl"]);
    } else {
        command.args(["-p", "hello"]);
    }
    let output = tokio::task::spawn_blocking(move || {
        acyclic_native_runtime::initialize_native_io_services().unwrap();
        acyclic_native_runtime::ProcessTree::spawn(&mut command)
            .unwrap()
            .wait_with_output(
                Duration::from_secs(6),
                65_536,
                None,
                || Ok(()),
                |_| Ok(acyclic_native_runtime::ProcessInput::default()),
                None,
                None,
            )
            .unwrap_or_else(|failure| panic!("GraphCoder process failed: {}", failure.error))
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

async fn scenario(jsonl: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (requests, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let counter = requests.clone();
    let app = Router::new()
        .route(
            "/count",
            post(move || {
                let requests = counter.clone();
                async move {
                    requests.send("count").unwrap();
                    Json(json!({"input_tokens":64}))
                }
            }),
        )
        .route(
            "/responses",
            post(move |Json(body): Json<Value>| {
                let requests = requests.clone();
                async move {
                    assert!(body.to_string().contains("hello"));
                    requests.send("model").unwrap();
                    ([("content-type", "text/event-stream")], ANSWER)
                }
            }),
        );
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let settings = directory.path().join("settings");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&settings).unwrap();
    let catalog = directory.path().join("catalog.json");
    std::fs::write(&catalog, serde_json::to_vec(&json!({"graphcoder-qual":{
        "adapter":"responses","authentication":"optional","base_url":format!("http://{address}"),
        "models":{"tiny":{"context_tokens":8192,"output_tokens":1024}},"max_event_bytes":4096,
        "token_counter":{"path":"count","adapter":"responses","response_field":"input_tokens","framing_tokens":0}
    }})).unwrap()).unwrap();
    let operation = OperationId::new();
    let output = execute(&workspace, &catalog, &settings, operation, jsonl).await;
    let mut lines = std::str::from_utf8(&output).unwrap().lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(header[0]["Session"], operation.to_string());
    let mut answers = Vec::new();
    for line in lines {
        let change: Change = serde_json::from_str(line).unwrap();
        for file in change.file.into_iter().chain(change.files) {
            if matches!(
                file.key,
                FileKey::Session {
                    file: TaskFile::ExecutionResult(_),
                    ..
                }
            ) {
                answers.push(serde_json::from_slice::<Value>(&file.bytes).unwrap());
            }
        }
    }
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["ok"]["text"], "fixture answer");
    assert_eq!(answers[0]["completion"], "completed");
    // A second real process restores the same request/result instead of generating again.
    execute(&workspace, &catalog, &settings, operation, jsonl).await;
    let mut generated = 0;
    let mut counted = 0;
    while let Ok(kind) = observed.try_recv() {
        match kind {
            "model" => generated += 1,
            "count" => counted += 1,
            _ => unreachable!(),
        }
    }
    assert_eq!(generated, 1);
    assert!(counted > 0);
    stop.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn prompt_argv_uses_the_real_registry_and_file_client() {
    scenario(false).await;
}

#[tokio::test]
async fn jsonl_stdin_eof_preserves_the_real_client_operation() {
    scenario(true).await;
}

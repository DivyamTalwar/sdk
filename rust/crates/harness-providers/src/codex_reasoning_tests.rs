use super::*;
use acyclic_fs::{CancellationToken, NativeMaterializationWork};
use acyclic_harness::model::{
    ModelContent, ModelContentPart, ModelEvent, ModelMessage, ModelRole, PreparedModelRequest,
    ToolResultContent,
};
use futures::StreamExt as _;
use std::sync::atomic::AtomicUsize;

const NATIVE: &str = r#"{ "type": "reasoning", "id": "rs_saved", "encrypted_content": "opaque-exact==", "summary": [] }"#;

#[tokio::test]
async fn encrypted_reasoning_survives_login_refresh_and_durable_tool_followup() -> Result<()> {
    for native in [false, true] {
        let first = format!(
            "data: {{\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{NATIVE}}}\n\n\
            data: {{\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{{\"type\":\"function_call\",\"call_id\":\"call_echo\",\"name\":\"echo\",\"arguments\":\"{{}}\"}}}}\n\n\
            data: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\",\"usage\":{{\"input_tokens\":5,\"output_tokens\":3}}}}}}\n\n"
        );
        let (_root, login, server, _) = fixture(vec![
            (200, first),
            (200, json!({"input_tokens":512}).to_string()),
            (401, String::new()),
            (
                200,
                json!({"access_token":"rotated-access", "refresh_token":"rotated-refresh"})
                    .to_string(),
            ),
            (200, RESPONSE.into()),
        ])?;
        let work = NativeMaterializationWork::new(
            acyclic_fs::performance::work_budget(65_536, 64 * 1024 * 1024),
            &CancellationToken::new(),
        )
        .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
        let baseline = work.live_allocation_bytes();
        let transport: Arc<dyn HttpTransport> = if native {
            Arc::new(super::native::NativeServer {
                inner: server.clone(),
                work: work.clone(),
                sent: AtomicUsize::new(0),
            })
        } else {
            server.clone()
        };
        let mut registry =
            Registry::with_transport(Credentials::explicit(BTreeMap::new()), transport)
                .with_codex_login(login);
        let mut spec = registry.catalog()["openai"].as_ref().clone();
        // A scripted contract, not an assertion about real backend endpoint support.
        spec.token_counter
            .as_mut()
            .expect("configured counter")
            .path = "fixture-count".into();
        registry.register("openai".into(), spec)?;
        let provider = registry.resolve("openai/gpt-5")?;
        let (prepared, initial) = crate::tests::request(&provider)?;
        let mut request = prepared.request().clone();
        request.tools.push(acyclic_harness::tool::ToolDefinition {
            name: "echo".into(),
            revision: "1".into(),
            description: "Echo".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            projection_schema: json!({"type":"object"}),
        });
        for step in 0..2 {
            let prepared = PreparedModelRequest::prepare(
                request.clone(),
                acyclic_harness::conversation::Limits::default(),
                None,
            )?;
            let dispatch = acyclic_harness::model::ModelDispatch {
                step,
                request_digest: prepared.request_digest(),
                ..initial
            };
            if step == 1 {
                let count = if native {
                    let (count, hold) = provider
                        .count_tokens_with_native_work(&prepared, &work)?
                        .await?;
                    assert!(hold.work().shares_allocation_context(&work));
                    drop(hold);
                    count
                } else {
                    provider.count_tokens(&prepared).await?
                };
                assert_eq!(count.input_tokens, 512);
            }
            let mut events = Vec::new();
            let mut holds = Vec::new();
            if native {
                let mut stream = provider
                    .generate_with_native_work(&prepared, dispatch, &work)?
                    .await?;
                while let Some(event) = stream.next().await {
                    let (event, hold) = event?;
                    events.push(event);
                    holds.push(hold);
                }
            } else {
                events = provider.generate(prepared, dispatch).try_collect().await?;
            }
            if step == 0 {
                let events: Vec<ModelEvent> =
                    serde_json::from_slice(&serde_json::to_vec(&events).expect("saved events"))
                        .expect("replayed events");
                let parts: Vec<_> = events
                    .into_iter()
                    .filter_map(|event| match event {
                        ModelEvent::ReasoningBlock { block } => {
                            assert_eq!(block.continuity, NATIVE.as_bytes());
                            Some(ModelContentPart::Reasoning { block })
                        }
                        ModelEvent::ToolCall(call) => Some(ModelContentPart::ToolCall(call)),
                        _ => None,
                    })
                    .collect();
                assert!(matches!(
                    &parts[..],
                    [
                        ModelContentPart::Reasoning { .. },
                        ModelContentPart::ToolCall(_)
                    ]
                ));
                request.messages.push(ModelMessage {
                    role: ModelRole::Assistant,
                    content: ModelContent::Parts(parts),
                });
                request.messages.push(ModelMessage {
                    role: ModelRole::Tool,
                    content: ModelContent::Part(ModelContentPart::ToolResult {
                        call_id: dispatch.tool_call_id(0),
                        name: "echo".into(),
                        content: ToolResultContent::Json {
                            value: json!({"ok":true}),
                        },
                    }),
                });
            } else {
                assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
            }
            drop(holds);
        }
        let mut requests = server.requests.lock().expect("recorded requests");
        assert_eq!(requests.len(), 5);
        assert!(requests[1].url.ends_with("/fixture-count"));
        assert_eq!(requests[1].headers["chatgpt-account-id"], "account");
        assert_eq!(
            crate::json(&requests[1].body)?["input"][1],
            crate::json(NATIVE.as_bytes())?
        );
        for index in [2, 4] {
            assert!(
                requests[index]
                    .url
                    .starts_with("https://chatgpt.com/backend-api/codex/")
            );
            assert!(
                requests[index]
                    .body
                    .windows(NATIVE.len())
                    .any(|bytes| bytes == NATIVE.as_bytes()),
                "native bytes survive the request serializer"
            );
            let body = crate::json(&requests[index].body)?;
            assert_eq!(body["instructions"], "system");
            assert_eq!(body["input"][1], crate::json(NATIVE.as_bytes())?);
            assert_eq!(body["input"][2]["type"], "function_call");
            assert_eq!(body["input"][3]["type"], "function_call_output");
            assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
            assert_eq!(body["store"], false);
        }
        assert_eq!(requests[2].body, requests[4].body);
        assert_eq!(
            requests[4].headers["authorization"],
            "Bearer rotated-access"
        );
        requests.clear();
        assert_eq!(work.live_allocation_bytes(), baseline);
    }
    Ok(())
}

use super::*;
use acyclic_harness::{
    Error,
    model::{ModelContentPart, ToolResultContent},
};
use std::collections::VecDeque;

fn total(count: &acyclic_harness::context::ModelTokenCount) -> u32 {
    count.fixed_tokens + count.message_tokens.iter().sum::<u32>()
}

#[tokio::test]
async fn token_estimates_start_at_three_bytes_and_calibrate_per_resolved_model() -> Result<()> {
    for (adapter, _) in fixtures() {
        let (provider, _) = provider(adapter, 200);
        let (prepared, dispatch) = request(&provider)?;
        let initial = total(&provider.count_tokens(&prepared)?);
        let bytes = serde_json::to_vec(&projection::body(&provider, prepared.request())?)
            .expect("encode fixture")
            .len();
        assert!(initial < bytes as u32 / 2 && initial >= bytes as u32 / 3);
        provider
            .generate(prepared.clone(), dispatch)
            .try_collect::<Vec<_>>()
            .await?;
        let calibrated = total(&provider.count_tokens(&prepared)?);
        assert!(
            calibrated < initial,
            "{adapter:?}: {initial} -> {calibrated}"
        );
        assert!(calibrated >= 5);
        let (other, _) = super::provider(adapter, 200);
        assert_eq!(total(&other.count_tokens(&prepared)?), initial);
    }
    Ok(())
}

#[tokio::test]
async fn http_status_classes_are_distinct_and_never_expose_response_payloads() -> Result<()> {
    for (adapter, _) in fixtures() {
        for status in [401, 403, 429, 500, 502, 503, 400, 404] {
            let (provider, _) = provider(adapter, status);
            let (prepared, dispatch) = request(&provider)?;
            let error = provider
                .generate(prepared, dispatch)
                .try_collect::<Vec<_>>()
                .await
                .expect_err("HTTP failure");
            assert!(error.to_string().contains(&status.to_string()));
            assert!(!error.to_string().contains("scripted-key"));
            assert!(!error.to_string().contains("usage"));
            match status {
                401 | 403 => assert!(matches!(error, Error::Unauthorized(_))),
                429 | 500..=599 => assert!(matches!(error, Error::Storage(_))),
                _ => assert!(matches!(error, Error::Invalid(_))),
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn interrupted_bodies_are_transient_and_protocol_violations_are_invalid() -> Result<()> {
    for (adapter, _) in fixtures() {
        for input in ["data: {}\n\n", "data: {", ""] {
            let error = stream::events(adapter, chunks(input, 1), 4096, parser_dispatch())
                .try_collect::<Vec<_>>()
                .await
                .expect_err("unfinished body");
            assert!(matches!(error, Error::Storage(_)), "{adapter:?}: {error}");
        }
        let error = stream::events(
            adapter,
            chunks("data: invalid\n\n", 1),
            4096,
            parser_dispatch(),
        )
        .try_collect::<Vec<_>>()
        .await
        .expect_err("bad JSON");
        assert!(matches!(error, Error::Invalid(_)));
        let interrupted =
            futures::stream::once(async { Err(Error::Storage("body interrupted".into())) });
        let error = stream::events(adapter, Box::pin(interrupted), 4096, parser_dispatch())
            .try_collect::<Vec<_>>()
            .await
            .expect_err("transport failure");
        assert!(matches!(error, Error::Storage(_)));
    }
    Ok(())
}

#[tokio::test]
async fn token_limits_complete_uniformly_without_publishing_partial_tool_batches() -> Result<()> {
    for (adapter, fixture) in fixtures() {
        let truncated = match adapter {
            Adapter::Chat => fixture.replace(
                "\"finish_reason\":\"tool_calls\"",
                "\"finish_reason\":\"length\"",
            ),
            Adapter::Anthropic => fixture.replace(
                "\"stop_reason\":\"tool_use\"",
                "\"stop_reason\":\"max_tokens\"",
            ),
            Adapter::Responses => fixture
                .replace("response.completed", "response.incomplete")
                .replace("\"status\":\"completed\"", "\"status\":\"incomplete\""),
            Adapter::Gemini => fixture.replace(
                "\"finishReason\":\"STOP\"",
                "\"finishReason\":\"MAX_TOKENS\"",
            ),
        };
        let events = stream::events(adapter, chunks(&truncated, 1), 4096, parser_dispatch())
            .try_collect::<Vec<_>>()
            .await?;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ModelEvent::ToolCall { .. }))
        );
        let Some(ModelEvent::Completed { metadata }) = events.last() else {
            return Err(invalid("no completion"));
        };
        assert_eq!(metadata["finish_reason"], "length");
        assert!(metadata["usage"].is_object());
    }
    let incomplete = concat!(
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"partial\",\"name\":\"echo\",\"arguments\":\"{\"}}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n"
    );
    let events = stream::events(
        Adapter::Responses,
        chunks(incomplete, 1),
        4096,
        parser_dispatch(),
    )
    .try_collect::<Vec<_>>()
    .await?;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], ModelEvent::Completed { .. }));
    Ok(())
}

struct ToolLoop {
    replies: Mutex<VecDeque<String>>,
    sent: Mutex<Vec<HttpRequest>>,
}

impl HttpTransport for ToolLoop {
    fn send(&self, request: HttpRequest) -> BoxProviderFuture<'_, Result<HttpResponse>> {
        Box::pin(async move {
            self.sent
                .lock()
                .map_err(|_| invalid("fixture lock"))?
                .push(request);
            let reply = self
                .replies
                .lock()
                .map_err(|_| invalid("fixture lock"))?
                .pop_front()
                .ok_or_else(|| invalid("unexpected dispatch"))?;
            Ok(HttpResponse {
                status: 200,
                body: chunks(&reply, 1),
            })
        })
    }
}

#[tokio::test]
async fn signed_thinking_survives_a_serialized_tool_call_and_followup_on_both_protocols()
-> Result<()> {
    for adapter in [Adapter::Anthropic, Adapter::Gemini] {
        let first = match adapter {
            Adapter::Anthropic => concat!(
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"signed\"}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"echo\",\"input\":{\"value\":1}}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            ),
            _ => {
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"echo\",\"args\":{\"value\":1}},\"thoughtSignature\":\"signed\"}]},\"finishReason\":\"STOP\"}]}\n\n"
            }
        };
        let second = match adapter {
            Adapter::Anthropic => {
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n"
            }
            _ => {
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"done\"}]},\"finishReason\":\"STOP\"}]}\n\n"
            }
        };
        let script = Arc::new(ToolLoop {
            replies: Mutex::new(VecDeque::from([first.into(), second.into()])),
            sent: Mutex::new(Vec::new()),
        });
        let (mut provider, _) = provider(adapter, 200);
        provider.transport = script.clone();
        let (initial, dispatch) = request(&provider)?;
        let events = provider
            .generate(initial.clone(), dispatch)
            .try_collect::<Vec<_>>()
            .await?;
        // Replay through the durable serde contract rather than an in-memory signature cache.
        let events: Vec<ModelEvent> =
            serde_json::from_slice(&serde_json::to_vec(&events).expect("encode events"))
                .expect("decode events");
        let call = events
            .into_iter()
            .find(|event| matches!(event, ModelEvent::ToolCall { .. }))
            .ok_or_else(|| invalid("missing tool call"))?;
        let ModelEvent::ToolCall {
            call_id,
            name,
            arguments,
            metadata,
        } = call
        else {
            unreachable!()
        };
        if adapter == Adapter::Gemini {
            assert_eq!(call_id, format!("{}-0-0", dispatch.operation_id));
        }
        let mut next = initial.request().clone();
        next.tools.push(acyclic_harness::tool::ToolDefinition {
            name: name.clone(),
            revision: "1".into(),
            description: "echo".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({}),
            projection_schema: json!({}),
        });
        next.messages.extend([
            ModelMessage {
                role: ModelRole::Assistant,
                content: ModelContent::Part(ModelContentPart::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments,
                    metadata,
                }),
            },
            ModelMessage {
                role: ModelRole::Tool,
                content: ModelContent::Part(ModelContentPart::ToolResult {
                    call_id,
                    name,
                    content: ToolResultContent::Json {
                        value: json!({"value":1}),
                    },
                }),
            },
        ]);
        let next = PreparedModelRequest::prepare(next, Limits::default(), None)?;
        let next = PreparedModelRequest::decode(next.bytes(), Limits::default(), None)?;
        let next_dispatch = ModelDispatch {
            step: 1,
            request_digest: next.request_digest(),
            ..dispatch
        };
        let events = provider
            .generate(next, next_dispatch)
            .try_collect::<Vec<_>>()
            .await?;
        assert_eq!(
            events[0],
            ModelEvent::Content {
                delta: "done".into()
            }
        );
        let sent = script.sent.lock().map_err(|_| invalid("fixture lock"))?;
        let body = json(&sent[1].body)?;
        if adapter == Adapter::Gemini {
            assert_eq!(
                body["contents"][1]["parts"][0]["thoughtSignature"],
                "signed"
            );
            assert_eq!(
                body["contents"][1]["parts"][0]["functionCall"]["id"],
                format!("{}-0-0", dispatch.operation_id)
            );
        } else {
            assert_eq!(
                body["messages"][1]["content"][0],
                json!({"type":"thinking","thinking":"plan","signature":"signed"})
            );
            assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
            assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        }
    }
    Ok(())
}

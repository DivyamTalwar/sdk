use super::*;
use crate::tool_names::Names;
use acyclic_harness::{
    model::{ModelContentPart, ToolCall, ToolResultContent},
    tool::ToolDefinition,
};
use proptest::prelude::*;
use std::collections::BTreeSet;

fn definition(name: String) -> ToolDefinition {
    ToolDefinition {
        name,
        revision: "1".into(),
        description: "Read a file".into(),
        input_schema: json!({"type":"object"}),
        output_schema: json!({"type":"object"}),
        projection_schema: json!({"type":"object"}),
    }
}

fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && (name.as_bytes()[0].is_ascii_alphabetic() || name.starts_with('_'))
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn assert_names(input: Vec<String>) {
    let (provider, _) = provider(Adapter::Responses, 200);
    let (prepared, _) = request(&provider).unwrap();
    let mut request = prepared.request().clone();
    request.tools = input
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(definition)
        .collect();
    Names::assert_round_trip(&request);
    let names = Names::new(&request);
    let mut seen = BTreeSet::new();
    for tool in &request.tools {
        let wire = names.wire(&tool.name).unwrap();
        assert!(valid(&wire), "target tool-name grammar");
        assert!(
            seen.insert(wire.to_string()),
            "distinct canonical names collided"
        );
        let mut call =
            ModelEvent::ToolCall(ToolCall::new("call".into(), wire.into_owned(), json!({})));
        names.restore(&mut call).unwrap();
        let ModelEvent::ToolCall(call) = call else {
            unreachable!()
        };
        assert_eq!(call.name, tool.name);
    }
    let mut permuted = request.clone();
    permuted.tools.reverse();
    let permuted_names = Names::new(&permuted);
    for tool in &request.tools {
        assert_eq!(
            names.wire(&tool.name).unwrap(),
            permuted_names.wire(&tool.name).unwrap()
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn arbitrary_canonical_names_have_collision_free_reversible_target_names(
        input in prop::collection::vec(".{0,128}", 0..24)
    ) { assert_names(input); }
}

#[test]
fn builtins_collisions_unicode_and_replayed_names_project_for_every_adapter() -> Result<()> {
    let canonical = [
        "acyclic.read_file",
        "acyclic_read_file",
        "harness_tool_0",
        "0start",
        "讀取",
        "echo",
    ];
    assert_names(canonical.into_iter().map(String::from).collect());
    for (adapter, _) in fixtures() {
        let (provider, _) = provider(adapter, 200);
        let (prepared, _) = request(&provider)?;
        let mut request = prepared.request().clone();
        request.tools = canonical
            .into_iter()
            .map(|name| definition(name.into()))
            .collect();
        request.messages.push(ModelMessage {
            role: ModelRole::Assistant,
            content: ModelContent::Parts(
                canonical
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        ModelContentPart::ToolCall(ToolCall::new(
                            format!("call_{i}"),
                            (*name).into(),
                            json!({}),
                        ))
                    })
                    .collect(),
            ),
        });
        request.messages.push(ModelMessage {
            role: ModelRole::Tool,
            content: ModelContent::Parts(
                canonical
                    .iter()
                    .enumerate()
                    .map(|(i, name)| ModelContentPart::ToolResult {
                        call_id: format!("call_{i}"),
                        name: (*name).into(),
                        content: ToolResultContent::Json { value: json!({}) },
                    })
                    .collect(),
            ),
        });
        let replay: ModelRequest =
            serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
        let body = projection::body(&provider, &request)?;
        assert_eq!(body, projection::body(&provider, &replay)?);
        let names = Names::new(&request);
        for name in canonical {
            let wire = names.wire(name)?;
            let encoded = serde_json::to_string(&body).unwrap();
            assert!(encoded.contains(&format!("\"name\":\"{wire}\"")));
            if name != wire {
                assert!(!encoded.contains(&format!("\"name\":\"{name}\"")));
            }
        }
    }
    Ok(())
}

#[test]
fn malformed_reserved_aliases_are_protocol_errors() {
    let (provider, _) = provider(Adapter::Responses, 200);
    let (prepared, _) = request(&provider).unwrap();
    let mut request = prepared.request().clone();
    request.tools.push(definition("acyclic.read_file".into()));
    let names = Names::new(&request);
    for wire in [
        "harness_tool_",
        "harness_tool_00",
        "harness_tool_+0",
        "harness_tool_99",
    ] {
        let mut event = ModelEvent::ToolCall(ToolCall::new("call".into(), wire.into(), json!({})));
        assert!(names.restore(&mut event).is_err());
    }
}

#[test]
#[should_panic(expected = "distinct canonical names collided")]
fn negative_control_rejects_lossy_punctuation_replacement() {
    let mut seen = BTreeSet::new();
    for name in ["acyclic.read_file", "acyclic_read_file"] {
        assert!(
            seen.insert(name.replace('.', "_")),
            "distinct canonical names collided"
        );
    }
}

#[tokio::test]
async fn every_adapter_restores_streamed_builtin_calls_with_native_custody() -> Result<()> {
    for (adapter, fixture) in [
        (
            Adapter::Chat,
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call\",\"function\":{\"name\":\"harness_tool_0\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
        ),
        (
            Adapter::Responses,
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call\",\"name\":\"harness_tool_0\",\"arguments\":\"{}\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        ),
        (
            Adapter::Anthropic,
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"harness_tool_0\",\"input\":{}}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
        (
            Adapter::Gemini,
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"call\",\"name\":\"harness_tool_0\",\"args\":{}}}]},\"finishReason\":\"STOP\"}]}\n\n",
        ),
    ] {
        let (mut provider, _) = provider(adapter, 200);
        provider.transport = Arc::new(native_tests::Fixture::new(fixture));
        let (prepared, _) = request(&provider)?;
        let mut request = prepared.request().clone();
        request.tools.push(definition("acyclic.read_file".into()));
        let prepared = PreparedModelRequest::prepare(request, Limits::default(), None)?;
        let dispatch = ModelDispatch {
            request_digest: prepared.request_digest(),
            ..parser_dispatch()
        };
        let work = native_tests::work(&acyclic_fs::CancellationToken::new())?;
        let baseline = work.live_allocation_bytes();
        let mut stream = provider
            .generate_with_native_work(&prepared, dispatch, &work)?
            .await?;
        let mut found = false;
        while let Some(event) = stream.next().await {
            let (event, custody) = event?;
            if let ModelEvent::ToolCall(call) = &event {
                assert_eq!(call.name, "acyclic.read_file");
                assert!(custody.bytes() >= event.logical_owned_bytes()?);
                found = true;
            }
        }
        drop(stream);
        assert!(found);
        assert_eq!(work.live_allocation_bytes(), baseline);
    }
    Ok(())
}

#[test]
fn checked_in_tool_name_seed_exercises_the_fuzzer_oracle() {
    let seed = include_bytes!("../fuzz/corpus/provider_stream/tool-names.json");
    let request: ModelRequest = serde_json::from_slice(&seed[1..]).unwrap();
    assert_eq!(request.tools[0].name, "acyclic.read_file");
    Names::assert_round_trip(&request);
    crate::fuzzing::assert_stream_invariants(seed);
}

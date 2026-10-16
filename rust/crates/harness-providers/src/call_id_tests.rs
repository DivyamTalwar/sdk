use crate::call_ids::CallIds;
use crate::{
    Adapter, projection,
    tests::{provider, request},
};
use acyclic_harness::model::{
    ModelContent, ModelContentPart, ModelMessage, ModelRequest, ModelRole, ToolCall,
    ToolResultContent,
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn history(input: &[String], adapter: Adapter) -> (crate::ResolvedModel, ModelRequest) {
    let (provider, _) = provider(adapter, 200);
    let (prepared, _) = request(&provider).unwrap();
    let mut request = prepared.request().clone();
    request.messages.push(ModelMessage {
        role: ModelRole::Assistant,
        content: ModelContent::Parts(
            input
                .iter()
                .map(|id| {
                    ModelContentPart::ToolCall(ToolCall::new(id.clone(), "echo".into(), json!({})))
                })
                .collect(),
        ),
    });
    request.messages.push(ModelMessage {
        role: ModelRole::Tool,
        content: ModelContent::Parts(
            input
                .iter()
                .map(|id| ModelContentPart::ToolResult {
                    call_id: id.clone(),
                    name: "echo".into(),
                    content: ToolResultContent::Json { value: json!({}) },
                })
                .collect(),
        ),
    });
    (provider, request)
}

fn projected_ids(body: &Value, adapter: Adapter) -> (Vec<String>, Vec<String>) {
    let mut calls = Vec::new();
    let mut results = Vec::new();
    fn push(out: &mut Vec<String>, value: &Value) {
        out.push(value.as_str().expect("projected ID").into());
    }
    match adapter {
        Adapter::Anthropic => {
            for message in body["messages"].as_array().unwrap() {
                for part in message["content"].as_array().unwrap() {
                    match part["type"].as_str() {
                        Some("tool_use") => push(&mut calls, &part["id"]),
                        Some("tool_result") => push(&mut results, &part["tool_use_id"]),
                        _ => {}
                    }
                }
            }
        }
        Adapter::Responses => {
            for part in body["input"].as_array().unwrap() {
                match part["type"].as_str() {
                    Some("function_call") => push(&mut calls, &part["call_id"]),
                    Some("function_call_output") => push(&mut results, &part["call_id"]),
                    _ => {}
                }
            }
        }
        Adapter::Chat => {
            for message in body["messages"].as_array().unwrap() {
                if let Some(parts) = message["tool_calls"].as_array() {
                    for part in parts {
                        push(&mut calls, &part["id"]);
                    }
                }
                if message["role"] == "tool" {
                    push(&mut results, &message["tool_call_id"]);
                }
            }
        }
        Adapter::Gemini => {
            for message in body["contents"].as_array().unwrap() {
                for part in message["parts"].as_array().unwrap() {
                    if let Some(call) = part.get("functionCall") {
                        push(&mut calls, &call["id"]);
                    }
                    if let Some(result) = part.get("functionResponse") {
                        push(&mut results, &result["id"]);
                    }
                }
            }
        }
    }
    (calls, results)
}

fn assert_ids(input: Vec<String>) {
    for adapter in [
        Adapter::Anthropic,
        Adapter::Responses,
        Adapter::Chat,
        Adapter::Gemini,
    ] {
        let (provider, request) = history(&input, adapter);
        CallIds::assert_round_trip(&request);
        let table = CallIds::new(&request);
        let expected: Vec<String> = input
            .iter()
            .map(|id| table.wire(id).unwrap().into_owned())
            .collect();
        let body = projection::body(&provider, &request).unwrap();
        let (calls, results) = projected_ids(&body, adapter);
        assert_eq!(calls, expected);
        assert_eq!(results, expected);
        let encoded = serde_json::to_vec(&request).unwrap();
        let replay = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(body, projection::body(&provider, &replay).unwrap());
        let mut permuted = request.clone();
        permuted.messages.reverse();
        let reordered = CallIds::new(&permuted);
        for id in &input {
            assert_eq!(table.wire(id).unwrap(), reordered.wire(id).unwrap());
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn arbitrary_ids_have_reversible_collision_free_call_and_result_bindings(
        input in prop::collection::vec(prop::collection::vec(any::<char>(), 0..96), 0..12)
    ) { assert_ids(input.into_iter().map(|chars| chars.into_iter().collect()).collect()); }
}

#[test]
fn punctuation_and_reserved_aliases_cannot_collide() {
    assert_ids(vec![
        "call:0".into(),
        "call/0".into(),
        "harness_call_0".into(),
        "call_0".into(),
        "界".repeat(40),
    ]);
    // Negative control: punctuation replacement loses canonical identity.
    let lossy = |id: &str| id.replace([':', '/'], "_");
    assert_eq!(lossy("call:0"), lossy("call/0"));
    let (_, request) = history(&["call:0".into(), "call/0".into()], Adapter::Anthropic);
    let table = CallIds::new(&request);
    assert_ne!(table.wire("call:0").unwrap(), table.wire("call/0").unwrap());
}

#[test]
fn checked_in_call_id_seed_exercises_the_shared_fuzzer_oracle() {
    let seed = include_bytes!("../fuzz/corpus/provider_stream/tool-call-ids.json");
    let _: ModelRequest = serde_json::from_slice(&seed[1..]).unwrap();
    crate::fuzzing::assert_stream_invariants(seed);
}

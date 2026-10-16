use super::*;
use acyclic_harness::model::{ModelContentPart, ToolCall, ToolResultContent};
use proptest::prelude::*;

fn values() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|value| json!(value)),
        prop::collection::vec(any::<char>(), 0..32)
            .prop_map(|chars| Value::String(chars.into_iter().collect()))
    ]
    .prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(".{0,32}", inner, 0..4)
                .prop_map(|values| Value::Object(values.into_iter().collect()))
        ]
    })
}

fn assert_projection(value: &Value) {
    crate::projection::arguments::assert_normalization(value);
    for adapter in [
        Adapter::Anthropic,
        Adapter::Responses,
        Adapter::Chat,
        Adapter::Gemini,
    ] {
        let (provider, _) = provider(adapter, 200);
        let (prepared, _) = request(&provider).unwrap();
        let mut request = prepared.request().clone();
        request.messages.extend([
            ModelMessage {
                role: ModelRole::Assistant,
                content: ModelContent::Part(ModelContentPart::ToolCall(ToolCall::new(
                    "call_0".into(),
                    "echo".into(),
                    value.clone(),
                ))),
            },
            ModelMessage {
                role: ModelRole::Tool,
                content: ModelContent::Part(ModelContentPart::ToolResult {
                    call_id: "call_0".into(),
                    name: "echo".into(),
                    content: ToolResultContent::Json {
                        value: json!("done"),
                    },
                }),
            },
        ]);
        let wire = crate::projection::body(&provider, &request).unwrap();
        let args = match adapter {
            Adapter::Anthropic => wire["messages"][1]["content"][0]["input"].clone(),
            Adapter::Gemini => wire["contents"][1]["parts"][0]["functionCall"]["args"].clone(),
            Adapter::Chat => serde_json::from_str(
                wire["messages"][2]["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
            Adapter::Responses => {
                serde_json::from_str(wire["input"][2]["arguments"].as_str().unwrap()).unwrap()
            }
        };
        assert!(args.is_object(), "{adapter:?}: non-object wire arguments");
        assert_eq!(
            if value.is_object() {
                &args
            } else {
                &args["_raw"]
            },
            value
        );
        assert_eq!(
            request.messages[2].content.parts()[0],
            ModelContentPart::ToolCall(ToolCall::new(
                "call_0".into(),
                "echo".into(),
                value.clone()
            ))
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn arbitrary_history_arguments_are_lossless_idempotent_objects_in_every_protocol(value in values()) { assert_projection(&value); }
}

#[test]
fn raw_marker_names_and_non_object_fixtures_are_preserved() {
    for value in [
        json!(null),
        json!(["line\n", 0, false]),
        json!("{}"),
        json!({"_raw":null}),
        json!({"$serde_json::private::RawValue":"null","nested":[]}),
    ] {
        assert_projection(&value);
    }
    // Negative control: sending any original non-object directly violates the
    // wire invariant; wrapping it twice would violate the idempotence oracle.
    let original = json!([1]);
    assert!(!original.is_object());
    assert_ne!(json!({"_raw":{"_raw":original}}), json!({"_raw":original}));
}

use super::*;

fn probes() -> Vec<(Adapter, Value, &'static str)> {
    let reasoning =
        json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque"});
    let answer = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]});
    let mut probes = vec![
        (
            Adapter::Responses,
            json!({"store":false,"input":[reasoning, {"role":"user","content":"interrupt"}]}),
            "OAR-RSN-02",
        ),
        (
            Adapter::Responses,
            json!({"store":false,"input":[reasoning, reasoning, answer]}),
            "OAR-RSN-02",
        ),
        (
            Adapter::Chat,
            json!({"messages":[{"role":"user","content":[{"type":"text"}]}]}),
            "OAC-SEQ-02",
        ),
        (
            Adapter::Responses,
            json!({"input":[{"type":"message","role":"assistant","content":[{"type":"output_text"}]}]}),
            "OAR-SEQ-03",
        ),
        (
            Adapter::Anthropic,
            json!({"messages":[{"role":"user","content":"question"},{"role":"assistant","content":"answer"}]}),
            "ANT-SEQ-06",
        ),
        (
            Adapter::Gemini,
            json!({"contents":[{"role":"model","parts":[{"text":"answer"}]}]}),
            "GEM-SEQ-05",
        ),
    ];
    let (_, mut chat, _) = fixtures()[2].clone();
    chat["messages"][1]["tool_calls"][0]
        .as_object_mut()
        .unwrap()
        .remove("type");
    probes.push((Adapter::Chat, chat, "OAC-TOOL-03"));
    for role in ["user", "assistant"] {
        let (_, mut anthropic, _) = fixtures()[0].clone();
        anthropic["messages"]
            .as_array_mut()
            .unwrap()
            .insert(2, json!({"role":role,"content":"interruption"}));
        probes.push((Adapter::Anthropic, anthropic, "ANT-TOOL-04"));
    }
    for (adapter, mut body, rule) in [
        (Adapter::Anthropic, fixtures()[0].1.clone(), "ANT-TOOL-07"),
        (Adapter::Responses, fixtures()[1].1.clone(), "OAR-TOOL-04"),
        (Adapter::Chat, fixtures()[2].1.clone(), "OAC-TOOL-01"),
    ] {
        match adapter {
            Adapter::Anthropic => {
                let calls = body["messages"][1]["content"].as_array_mut().unwrap();
                calls.push(calls[1].clone());
            }
            Adapter::Responses => {
                let calls = body["input"].as_array_mut().unwrap();
                calls.insert(2, calls[1].clone());
            }
            Adapter::Chat => {
                let calls = body["messages"][1]["tool_calls"].as_array_mut().unwrap();
                calls.push(calls[0].clone());
            }
            Adapter::Gemini => unreachable!(),
        }
        probes.push((adapter, body, rule));
    }
    probes.push((
        Adapter::Responses,
        json!({"input":"hello","tools":[{"type":"function","name":"bad name"}]}),
        "OAR-TOOL-01",
    ));
    probes
}

fn assert_rule(adapter: Adapter, body: &Value, rule: &'static str, omit: bool) {
    let mut errors = validate_input(adapter, body);
    if omit {
        errors.retain(|error| *error != rule);
    }
    assert!(errors.contains(&rule), "missing {rule} for {adapter:?}");
}

#[test]
fn required_successors_fields_and_portable_boundaries_are_checked() {
    for (adapter, body, rule) in probes() {
        assert_rule(adapter, &body, rule, false);
    }
}

#[test]
fn omitting_each_adjacency_shape_or_boundary_check_fails_the_same_oracle() {
    for (adapter, body, rule) in probes() {
        assert!(
            std::panic::catch_unwind(|| assert_rule(adapter, &body, rule, true)).is_err(),
            "negative control did not detect missing {rule}"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn content_shape_survives_codec_and_rejects_missing_or_wrong_text(chars in prop::collection::vec(any::<char>(), 0..64)) {
        let text: String = chars.into_iter().collect();
        for (adapter, mut body, pointer, rule) in [
            (Adapter::Chat, json!({"messages":[{"role":"user","content":[{"type":"text","text":text}]}]}), "/messages/0/content/0/text", "OAC-SEQ-02"),
            (Adapter::Responses, json!({"input":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}), "/input/0/content/0/text", "OAR-SEQ-03"),
        ] {
            prop_assert!(validate_input(adapter, &body).is_empty());
            let replay = serde_json::from_slice(&serde_json::to_vec(&body).unwrap()).unwrap();
            prop_assert_eq!(validate_input(adapter, &body), validate_input(adapter, &replay));
            *body.pointer_mut(pointer).unwrap() = json!(false);
            prop_assert!(validate_input(adapter, &body).contains(&rule));
        }
    }
}

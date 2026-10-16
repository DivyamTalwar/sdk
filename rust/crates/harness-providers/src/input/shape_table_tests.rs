//! Every declared shape is exercised in every declared content position.
use super::*;
use crate::protocol::{
    content,
    shape::{self, Constraint, Shape},
};

#[derive(Clone, Copy, Debug)]
enum Position {
    Chat(&'static str),
    Responses(&'static str),
    Output,
    ChatCall,
    ResponsesCall,
    Native(Adapter),
}

fn entries() -> Vec<(Position, &'static Shape)> {
    let mut entries = Vec::new();
    for (roles, position) in [
        (
            content::CHAT,
            Position::Chat as fn(&'static str) -> Position,
        ),
        (
            content::RESPONSES,
            Position::Responses as fn(&'static str) -> Position,
        ),
    ] {
        for (role, content) in roles {
            entries.extend(content.parts.iter().map(|shape| (position(role), *shape)));
        }
    }
    entries.extend(
        content::RESPONSE_TOOL_OUTPUT
            .parts
            .iter()
            .map(|shape| (Position::Output, *shape)),
    );
    entries.extend([
        (Position::ChatCall, &shape::CHAT_CALL),
        (Position::ResponsesCall, &shape::RESPONSE_CALL),
        (Position::Native(Adapter::Anthropic), &shape::THINKING),
        (Position::Native(Adapter::Anthropic), &shape::REDACTED),
        (Position::Native(Adapter::Responses), &shape::REASONING),
    ]);
    entries
}

fn insert(value: &mut Value, pointer: &str, field: Value) {
    let mut path = pointer.trim_start_matches('/').split('/').peekable();
    let mut node = value;
    while let Some(key) = path.next() {
        if path.peek().is_none() {
            node.as_object_mut().unwrap().insert(key.into(), field);
            return;
        }
        node = node
            .as_object_mut()
            .unwrap()
            .entry(key)
            .or_insert_with(|| json!({}));
    }
}

fn example(shape: &Shape, text: &str) -> Value {
    let mut value = json!({shape.discriminator.0: shape.discriminator.1});
    for (path, constraint) in shape.fields {
        let field = match constraint {
            Constraint::String { nonempty } => json!(if *nonempty {
                format!("x{text}")
            } else {
                text.into()
            }),
            Constraint::Name(_) => json!("call_1"),
            Constraint::ObjectJson => json!("{}"),
            Constraint::Array => {
                json!([{"type":"summary_text","text":text,"nested":[{"key":text}]}])
            }
        };
        insert(&mut value, path, field);
    }
    value
}

fn admitted(position: Position, part: &Value) -> bool {
    let (adapter, body) = match position {
        Position::Chat("tool") => {
            let mut body = fixtures()[2].1.clone();
            body["messages"][2]["content"] = json!([part]);
            (Adapter::Chat, body)
        }
        Position::Chat(role) => (
            Adapter::Chat,
            json!({"messages":[{"role":role,"content":[part]}]}),
        ),
        Position::Responses(role) => (
            Adapter::Responses,
            json!({"input":[{"type":"message","role":role,"content":[part]}]}),
        ),
        Position::Output => {
            let mut body = fixtures()[1].1.clone();
            body["input"][2]["output"] = json!([part]);
            (Adapter::Responses, body)
        }
        Position::ChatCall => {
            let mut body = fixtures()[2].1.clone();
            body["messages"][1]["tool_calls"] = json!([part]);
            (Adapter::Chat, body)
        }
        Position::ResponsesCall => {
            let mut body = fixtures()[1].1.clone();
            body["input"][1] = part.clone();
            (Adapter::Responses, body)
        }
        Position::Native(adapter) => {
            return crate::projection::native_reasoning::validated(
                adapter,
                &serde_json::to_vec(part).unwrap(),
            )
            .is_some();
        }
    };
    validate_input(adapter, &body).is_empty()
}

fn assert_entry(position: Position, shape: &Shape, text: &str, omit_fields: bool) {
    let value = example(shape, text);
    assert!(
        shape.accepts(&value),
        "bad generated fixture for {}",
        shape.discriminator.1
    );
    assert!(
        admitted(position, &value),
        "bad position fixture: {position:?} {value}"
    );
    let encoded = serde_json::to_vec(&value).unwrap();
    let replay: Value = serde_json::from_slice(&encoded).unwrap();
    assert!(admitted(position, &replay));
    // Negative control removes the required-field data, retaining the very same
    // discriminator. The mutation oracle below must detect every weakened row.
    let rule = Shape {
        discriminator: shape.discriminator,
        fields: if omit_fields { &[] } else { shape.fields },
    };
    for path in std::iter::once(format!("/{}", shape.discriminator.0))
        .chain(shape.fields.iter().map(|(path, _)| (*path).into()))
    {
        let mut missing = value.clone();
        let (parent, key) = path.rsplit_once('/').unwrap();
        missing
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            !rule.accepts(&missing),
            "missing {path}: {position:?} {missing}"
        );
        assert!(
            !admitted(position, &missing),
            "missing {path}: {position:?} {missing}"
        );
        for wrong in [json!(null), json!(true), json!(42), json!({}), json!([])] {
            let mut broken = value.clone();
            *broken.pointer_mut(&path).unwrap() = wrong;
            // [] is a valid array, not a wrong type for an array requirement.
            if shape.accepts(&broken) {
                continue;
            }
            assert!(!rule.accepts(&broken), "wrong type {path}: {position:?}");
            assert!(
                !admitted(position, &broken),
                "wrong type {path}: {position:?} {broken}"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn every_shape_rejects_each_missing_or_wrongly_typed_required_field(
        chars in prop::collection::vec(any::<char>(), 0..32),
    ) {
        let text: String = chars.into_iter().collect();
        for (position, shape) in entries() {
            assert_entry(position, shape, &text, false);
        }
    }
}

#[test]
fn ignoring_required_fields_breaks_every_shape_entry() {
    for (position, shape) in entries() {
        assert!(
            std::panic::catch_unwind(|| assert_entry(position, shape, "line\nquoted\"", true))
                .is_err()
        );
    }
}

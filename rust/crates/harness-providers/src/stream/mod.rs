mod anthropic;
mod chat;
mod gemini;
mod responses;

use crate::{Adapter, invalid, json, sse};
use acyclic_harness::{
    Result,
    model::{ModelDispatch, ModelEvent},
};
use acyclic_stream::BoxProviderStream;
use bytes::Bytes;
use futures::StreamExt as _;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
    metadata: Value,
}

#[derive(Default)]
struct State {
    calls: BTreeMap<u64, Call>,
    metadata: serde_json::Map<String, Value>,
    terminal: bool,
    thinking: BTreeMap<u64, Value>,
}

pub(crate) fn events<'a>(
    adapter: Adapter,
    body: BoxProviderStream<'a, Result<Bytes>>,
    maximum: usize,
    dispatch: ModelDispatch,
) -> BoxProviderStream<'a, Result<ModelEvent>> {
    Box::pin(async_stream::try_stream! {
        let mut input = sse::decode(body, maximum);
        let mut state = State::default();
        while let Some(data) = input.next().await {
            let data = data?;
            if data == "[DONE]" {
                if adapter != Adapter::Chat { Err(invalid("unexpected stream terminator"))?; }
                state.terminal = true;
                break;
            }
            let value = crate::json(data.as_bytes())?;
            if !value.is_object() { Err(invalid("model event must be an object"))?; }
            if value.get("error").is_some_and(|error| !error.is_null())
                || matches!(value["type"].as_str(), Some("error" | "response.failed")) {
                Err(invalid("model provider reported a stream error"))?;
            }
            let deltas = match adapter {
                Adapter::Chat => chat::parse(&mut state, &value, maximum)?,
                Adapter::Anthropic => anthropic::parse(&mut state, &value, maximum)?,
                Adapter::Responses => responses::parse(&mut state, &value, maximum)?,
                Adapter::Gemini => gemini::parse(&mut state, &value, maximum, dispatch)?,
            };
            for event in deltas { yield event; }
            if state.terminal && matches!(adapter, Adapter::Anthropic | Adapter::Responses) { break; }
        }
        if !state.terminal { Err(acyclic_harness::Error::Storage("model stream ended without a terminal marker".into()))?; }
        if state.metadata.get("finish_reason").is_some_and(|reason| reason == "length") {
            yield ModelEvent::Completed { metadata: Value::Object(state.metadata) };
            return;
        }
        // Validate the whole tool batch before exposing any call to the executor.
        let mut seen = BTreeSet::new();
        let mut calls = Vec::with_capacity(state.calls.len());
        for (position, mut call) in state.calls.into_values().enumerate() {
            if call.id.trim().is_empty() || call.name.trim().is_empty() || !seen.insert(call.id.clone()) {
                Err(invalid("incomplete or duplicate model tool call"))?;
            }
            let arguments = json(call.arguments.as_bytes())?;
            if !arguments.is_object() { Err(invalid("tool arguments must be a JSON object"))?; }
            validate_numbers(&arguments)?;
            if adapter == Adapter::Anthropic && position == 0 && !state.thinking.is_empty() {
                call.metadata = json!({"anthropic_thinking": state.thinking.values().collect::<Vec<_>>()});
            }
            calls.push(ModelEvent::ToolCall { call_id: call.id, name: call.name, arguments, metadata: call.metadata });
        }
        for call in calls { yield call; }
        yield ModelEvent::Completed { metadata: Value::Object(state.metadata) };
    })
}

impl State {
    fn usage(&mut self, usage: &Value) {
        let Some(object) = usage.as_object() else {
            return;
        };
        let retained = self.metadata.entry("usage").or_insert_with(|| json!({}));
        if let Some(map) = retained.as_object_mut() {
            map.extend(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
    }

    fn identity(&mut self, value: &Value) {
        for field in ["id", "model"] {
            if let Some(value) = value.get(field) {
                self.metadata.insert(field.into(), value.clone());
            }
        }
    }
}

fn text(events: &mut Vec<ModelEvent>, value: &Value, reasoning: bool) {
    if let Some(delta) = value.as_str().filter(|delta| !delta.is_empty()) {
        events.push(if reasoning {
            ModelEvent::Reasoning {
                delta: delta.into(),
            }
        } else {
            ModelEvent::Content {
                delta: delta.into(),
            }
        });
    }
}

fn index(value: &Value, field: &str) -> Result<u64> {
    value[field]
        .as_u64()
        .filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or_else(|| invalid("invalid tool index"))
}

fn string(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| invalid("missing tool identity or string"))
}

fn append(target: &mut String, value: Option<&Value>, maximum: usize) -> Result<()> {
    if let Some(value) = value {
        let delta = value
            .as_str()
            .ok_or_else(|| invalid("tool delta must be a string"))?;
        if delta.len() > maximum.saturating_sub(target.len()) {
            return Err(invalid("tool call exceeds configured byte limit"));
        }
        target.push_str(delta);
    }
    Ok(())
}

fn validate_numbers(value: &Value) -> Result<()> {
    match value {
        Value::Number(number)
            if number
                .as_f64()
                .is_none_or(|number| number.abs() > 9_007_199_254_740_991.0) =>
        {
            Err(invalid("tool number exceeds the portable exact range"))
        }
        Value::Array(values) => values.iter().try_for_each(validate_numbers),
        Value::Object(values) => values.values().try_for_each(validate_numbers),
        _ => Ok(()),
    }
}

mod anthropic;
mod budget;
mod chat;
mod gemini;
#[cfg(test)]
mod limits_tests;
mod responses;

use crate::{Adapter, invalid, json, sse};
use acyclic_harness::{
    Result,
    model::{ModelDispatch, ModelEvent},
};
use acyclic_stream::BoxProviderStream;
use bytes::Bytes;
use futures::StreamExt as _;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct Call {
    id: String,
    name: String,
    arguments: String,
    metadata: Value,
}

pub(crate) struct State<'work> {
    calls: BTreeMap<u64, Call>,
    metadata: serde_json::Map<String, Value>,
    pub(crate) terminal: bool,
    thinking: BTreeMap<u64, Value>,
    budget: budget::Budget<'work>,
}

#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "fuzzing")))]
pub(crate) fn events<'a>(
    adapter: Adapter,
    body: BoxProviderStream<'a, Result<Bytes>>,
    maximum: usize,
    dispatch: ModelDispatch,
) -> BoxProviderStream<'a, Result<ModelEvent>> {
    events_named(adapter, body, maximum, dispatch, None)
}

pub(crate) fn events_named<'a>(
    adapter: Adapter,
    body: BoxProviderStream<'a, Result<Bytes>>,
    maximum: usize,
    dispatch: ModelDispatch,
    names: Option<&'a crate::tool_names::Names<'a>>,
) -> BoxProviderStream<'a, Result<ModelEvent>> {
    Box::pin(async_stream::try_stream! {
        let mut input = sse::decode(body, maximum);
        let mut state = State::new(maximum);
        while let Some(data) = input.next().await {
            let data = data?;
            let deltas = parse(&mut state, adapter, &data, dispatch)?;
            for event in deltas { yield event; }
            if state.terminal && (data == "[DONE]" || matches!(adapter, Adapter::Anthropic | Adapter::Responses)) { break; }
        }
        if let Some(names) = names { state.restore_names(names, |_| Ok(()))?; }
        let events = finish(state, adapter)?;
        for event in events { yield event; }
    })
}

pub(crate) fn parse(
    state: &mut State,
    adapter: Adapter,
    data: &str,
    dispatch: ModelDispatch,
) -> Result<Vec<ModelEvent>> {
    if data == "[DONE]" {
        if adapter != Adapter::Chat {
            return Err(invalid("unexpected stream terminator"));
        }
        state.terminal = true;
        return Ok(Vec::new());
    }
    let value = crate::json(data.as_bytes())?;
    if !value.is_object() {
        return Err(invalid("model event must be an object"));
    }
    if value.get("error").is_some_and(|error| !error.is_null())
        || matches!(value["type"].as_str(), Some("error" | "response.failed"))
    {
        return Err(invalid("model provider reported a stream error"));
    }
    match adapter {
        Adapter::Chat => chat::parse(state, &value),
        Adapter::Anthropic => anthropic::parse(state, &value),
        Adapter::Responses => responses::parse(state, &value),
        Adapter::Gemini => gemini::parse(state, &value, dispatch),
    }
}

pub(crate) fn finish(mut state: State, adapter: Adapter) -> Result<Vec<ModelEvent>> {
    if !state.terminal {
        return Err(acyclic_harness::Error::Storage(
            "model stream ended without a terminal marker".into(),
        ));
    }
    let mut calls = Vec::with_capacity(state.calls.len() + 1);
    if state
        .metadata
        .get("finish_reason")
        .is_some_and(|reason| reason == "length")
    {
        calls.push(ModelEvent::Completed {
            metadata: Value::Object(state.metadata),
        });
        return Ok(calls);
    }
    // Validate the whole tool batch before exposing any call to the executor.
    let mut seen = BTreeSet::new();
    for (position, mut call) in state.calls.into_values().enumerate() {
        if call.id.trim().is_empty() || call.name.trim().is_empty() || !seen.insert(call.id.clone())
        {
            return Err(invalid("incomplete or duplicate model tool call"));
        }
        let arguments = json(call.arguments.as_bytes())?;
        if !arguments.is_object() {
            return Err(invalid("tool arguments must be a JSON object"));
        }
        validate_numbers(&arguments)?;
        if matches!(adapter, Adapter::Anthropic | Adapter::Responses)
            && position == 0
            && !state.thinking.is_empty()
        {
            let key = if adapter == Adapter::Anthropic {
                "anthropic_thinking"
            } else {
                "openai_reasoning"
            };
            state.budget.add(
                std::mem::size_of::<(String, Value)>()
                    + key.len()
                    + state.thinking.len() * std::mem::size_of::<Value>(),
                state.thinking.len() + 1,
            )?;
            call.metadata = Value::Object(serde_json::Map::from_iter([(
                key.into(),
                Value::Array(std::mem::take(&mut state.thinking).into_values().collect()),
            )]));
        }
        calls.push(ModelEvent::ToolCall(
            acyclic_harness::model::ToolCall::new(call.id, call.name, arguments)
                .with_metadata(call.metadata),
        ));
    }
    calls.push(ModelEvent::Completed {
        metadata: Value::Object(state.metadata),
    });
    Ok(calls)
}

impl<'work> State<'work> {
    pub(crate) fn restore_names(
        &mut self,
        names: &crate::tool_names::Names,
        mut fund: impl FnMut(usize) -> Result<()>,
    ) -> Result<()> {
        if self
            .metadata
            .get("finish_reason")
            .is_some_and(|reason| reason == "length")
        {
            return Ok(());
        }
        for call in self.calls.values_mut() {
            if let Some(name) = names.canonical(&call.name)? {
                fund(name.len())?;
                self.budget.string(&mut call.name, name)?;
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn logical_owned_bytes(&self) -> Result<u64> {
        use acyclic_harness::model::json_value_logical_owned_bytes as owned;
        let mut bytes = std::mem::size_of::<Self>() as u64;
        for call in self.calls.values() {
            bytes += std::mem::size_of::<(u64, Call)>() as u64
                + call.id.capacity() as u64
                + call.name.capacity() as u64
                + call.arguments.capacity() as u64
                + owned(&call.metadata)?;
        }
        for (key, value) in &self.metadata {
            bytes += std::mem::size_of::<(String, Value)>() as u64
                + key.capacity() as u64
                + owned(value)?;
        }
        for value in self.thinking.values() {
            bytes += std::mem::size_of::<(u64, Value)>() as u64 + owned(value)?;
        }
        Ok(bytes)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn arguments(&self) -> impl Iterator<Item = &str> {
        self.calls.values().map(|call| call.arguments.as_str())
    }

    pub(crate) fn new(maximum: usize) -> Self {
        Self {
            calls: BTreeMap::new(),
            metadata: serde_json::Map::new(),
            terminal: false,
            thinking: BTreeMap::new(),
            budget: budget::Budget::new(maximum),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn native(
        maximum: usize,
        work: &'work acyclic_fs::NativeMaterializationWork,
    ) -> Self {
        Self {
            budget: budget::Budget::native(maximum, work),
            ..Self::new(maximum)
        }
    }

    fn call(&mut self, index: u64) -> Result<(&mut Call, &mut budget::Budget<'work>)> {
        let call = match self.calls.entry(index) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                self.budget.add(std::mem::size_of::<(u64, Call)>(), 1)?;
                entry.insert(Call::default())
            }
        };
        Ok((call, &mut self.budget))
    }

    fn retain_thinking(&mut self, index: u64, value: &Value) -> Result<()> {
        let target = match self.thinking.entry(index) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                self.budget.add(std::mem::size_of::<(u64, Value)>(), 1)?;
                entry.insert(Value::Null)
            }
        };
        self.budget.value(target, value)
    }

    fn thinking_delta(&mut self, index: u64, key: &str, value: &Value) -> Result<()> {
        let block = match self.thinking.entry(index) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                self.budget.add(std::mem::size_of::<(u64, Value)>(), 1)?;
                let mut map = serde_json::Map::new();
                self.budget.member_str(&mut map, "type", "thinking")?;
                self.budget.member_str(&mut map, "thinking", "")?;
                self.budget.member_str(&mut map, "signature", "")?;
                entry.insert(Value::Object(map))
            }
        };
        let map = block
            .as_object_mut()
            .ok_or_else(|| invalid("invalid thinking block"))?;
        if !map.contains_key(key) {
            self.budget.member_str(map, key, "")?;
        }
        let Some(Value::String(text)) = map.get_mut(key) else {
            return Err(invalid("invalid thinking string"));
        };
        self.budget.append(text, Some(value))
    }

    fn usage(&mut self, usage: &Value) -> Result<()> {
        let Some(object) = usage.as_object() else {
            return Ok(());
        };
        if !self.metadata.contains_key("usage") {
            self.budget.member(
                &mut self.metadata,
                "usage",
                &Value::Object(serde_json::Map::new()),
            )?;
        }
        let retained = self
            .metadata
            .get_mut("usage")
            .ok_or_else(|| invalid("missing usage"))?;
        if let Some(map) = retained.as_object_mut() {
            for (key, value) in object {
                self.budget.member(map, key, value)?;
            }
        }
        Ok(())
    }

    fn identity(&mut self, value: &Value) -> Result<()> {
        for field in ["id", "model"] {
            if let Some(value) = value.get(field) {
                self.budget.member(&mut self.metadata, field, value)?;
            }
        }
        Ok(())
    }
    fn finish_reason(&mut self, reason: &str) -> Result<()> {
        self.budget
            .member_str(&mut self.metadata, "finish_reason", reason)
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

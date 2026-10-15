use crate::{Adapter, invalid, json, sse};
use acyclic_harness::{Result, model::ModelEvent};
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
}

#[derive(Default)]
struct State {
    calls: BTreeMap<u64, Call>,
    metadata: serde_json::Map<String, Value>,
    terminal: bool,
}

pub(crate) fn events<'a>(
    adapter: Adapter,
    body: BoxProviderStream<'a, Result<Bytes>>,
    maximum: usize,
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
                || matches!(value["type"].as_str(), Some("error" | "response.failed" | "response.incomplete")) {
                Err(invalid("model provider reported a stream error"))?;
            }
            let deltas = match adapter {
                Adapter::Chat => state.chat(&value, maximum)?,
                Adapter::Anthropic => state.anthropic(&value, maximum)?,
                Adapter::Responses => state.responses(&value, maximum)?,
                Adapter::Gemini => state.gemini(&value, maximum)?,
            };
            for event in deltas { yield event; }
            if state.terminal && matches!(adapter, Adapter::Anthropic | Adapter::Responses) { break; }
        }
        if !state.terminal { Err(invalid("model stream ended without a terminal marker"))?; }
        // Validate the whole tool batch before exposing any call to the executor.
        let mut seen = BTreeSet::new();
        let mut calls = Vec::with_capacity(state.calls.len());
        for call in state.calls.into_values() {
            if call.id.trim().is_empty() || call.name.trim().is_empty() || !seen.insert(call.id.clone()) {
                Err(invalid("incomplete or duplicate model tool call"))?;
            }
            let arguments = json(call.arguments.as_bytes())?;
            if !arguments.is_object() { Err(invalid("tool arguments must be a JSON object"))?; }
            validate_numbers(&arguments)?;
            calls.push(ModelEvent::ToolCall { call_id: call.id, name: call.name, arguments });
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

    fn chat(&mut self, value: &Value, maximum: usize) -> Result<Vec<ModelEvent>> {
        self.identity(value);
        self.usage(&value["usage"]);
        let Some(choice) = value["choices"]
            .as_array()
            .and_then(|values| values.first())
        else {
            return Ok(Vec::new());
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            if matches!(reason, "content_filter" | "error") {
                return Err(invalid("chat generation was blocked"));
            }
            self.metadata.insert("finish_reason".into(), json!(reason));
            self.terminal = true;
        }
        let delta = &choice["delta"];
        let mut events = Vec::new();
        text(&mut events, &delta["content"], false);
        text(&mut events, &delta["reasoning_content"], true);
        if let Some(calls) = delta["tool_calls"].as_array() {
            for raw in calls {
                let index = index(raw, "index")?;
                if raw.get("type").is_some_and(|kind| kind != "function") {
                    return Err(invalid("unsupported chat tool type"));
                }
                let call = self.calls.entry(index).or_default();
                if let Some(id) = raw.get("id") {
                    let id = string(id)?;
                    if !call.id.is_empty() && call.id != id {
                        return Err(invalid("tool call ID changed"));
                    }
                    call.id = id.into();
                }
                if let Some(function) = raw.get("function") {
                    if !function.is_object() {
                        return Err(invalid("tool function must be an object"));
                    }
                    append(&mut call.name, function.get("name"), maximum)?;
                    append(&mut call.arguments, function.get("arguments"), maximum)?;
                }
            }
        }
        Ok(events)
    }

    fn anthropic(&mut self, value: &Value, maximum: usize) -> Result<Vec<ModelEvent>> {
        let mut events = Vec::new();
        match value["type"].as_str() {
            Some("message_start") => {
                self.identity(&value["message"]);
                self.usage(&value["message"]["usage"]);
            }
            Some("content_block_start") => {
                let block = &value["content_block"];
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let call = Call {
                            id: string(&block["id"])?.into(),
                            name: string(&block["name"])?.into(),
                            arguments: if block["input"]
                                .as_object()
                                .is_some_and(|map| !map.is_empty())
                            {
                                block["input"].to_string()
                            } else {
                                String::new()
                            },
                        };
                        if self.calls.insert(index(value, "index")?, call).is_some() {
                            return Err(invalid("repeated tool block"));
                        }
                    }
                    Some("text") => text(&mut events, &block["text"], false),
                    Some("thinking") => text(&mut events, &block["thinking"], true),
                    _ => {}
                }
            }
            Some("content_block_delta") => {
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => text(&mut events, &delta["text"], false),
                    Some("thinking_delta") => text(&mut events, &delta["thinking"], true),
                    Some("input_json_delta") => {
                        let call = self
                            .calls
                            .get_mut(&index(value, "index")?)
                            .ok_or_else(|| invalid("tool delta without block"))?;
                        append(&mut call.arguments, delta.get("partial_json"), maximum)?;
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                if let Some(call) = self.calls.get_mut(&index(value, "index")?) {
                    if call.arguments.is_empty() {
                        call.arguments = "{}".into();
                    }
                }
            }
            Some("message_delta") => {
                self.usage(&value["usage"]);
                if let Some(reason) = value["delta"].get("stop_reason") {
                    self.metadata.insert("finish_reason".into(), reason.clone());
                }
            }
            Some("message_stop") => self.terminal = true,
            _ => {}
        }
        Ok(events)
    }

    fn responses(&mut self, value: &Value, maximum: usize) -> Result<Vec<ModelEvent>> {
        let mut events = Vec::new();
        match value["type"].as_str() {
            Some("response.output_text.delta" | "response.refusal.delta") => {
                text(&mut events, &value["delta"], false)
            }
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
                text(&mut events, &value["delta"], true)
            }
            Some("response.output_item.added" | "response.output_item.done") => {
                let item = &value["item"];
                if item["type"] == "function_call" {
                    let call = self.calls.entry(index(value, "output_index")?).or_default();
                    call.id = string(&item["call_id"])?.into();
                    call.name = string(&item["name"])?.into();
                    if let Some(arguments) = item["arguments"].as_str() {
                        if arguments.len() > maximum {
                            return Err(invalid("tool arguments exceed configured limit"));
                        }
                        call.arguments = arguments.into();
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                let call = self
                    .calls
                    .get_mut(&index(value, "output_index")?)
                    .ok_or_else(|| invalid("tool delta without item"))?;
                append(&mut call.arguments, value.get("delta"), maximum)?;
            }
            Some("response.function_call_arguments.done") => {
                let call = self
                    .calls
                    .get_mut(&index(value, "output_index")?)
                    .ok_or_else(|| invalid("tool done without item"))?;
                call.arguments.clear();
                append(&mut call.arguments, value.get("arguments"), maximum)?;
            }
            Some("response.completed") => {
                let response = &value["response"];
                if response["status"]
                    .as_str()
                    .is_some_and(|status| status != "completed")
                {
                    return Err(invalid("Responses generation did not complete"));
                }
                self.identity(response);
                self.usage(&response["usage"]);
                self.terminal = true;
            }
            _ => {}
        }
        Ok(events)
    }

    fn gemini(&mut self, value: &Value, maximum: usize) -> Result<Vec<ModelEvent>> {
        self.usage(&value["usageMetadata"]);
        if let Some(id) = value.get("responseId") {
            self.metadata.insert("id".into(), id.clone());
        }
        if value["promptFeedback"].get("blockReason").is_some() {
            return Err(invalid("Gemini prompt blocked"));
        }
        let Some(candidate) = value["candidates"]
            .as_array()
            .and_then(|values| values.first())
        else {
            return Ok(Vec::new());
        };
        let mut events = Vec::new();
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for part in parts {
                text(&mut events, &part["text"], part["thought"] == true);
                if let Some(function) = part.get("functionCall") {
                    let index = self.calls.len() as u64;
                    let arguments = function["args"].to_string();
                    if arguments.len() > maximum {
                        return Err(invalid("tool arguments exceed configured limit"));
                    }
                    self.calls.insert(
                        index,
                        Call {
                            id: function["id"]
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("gemini-{index}")),
                            name: string(&function["name"])?.into(),
                            arguments,
                        },
                    );
                }
            }
        }
        if let Some(reason) = candidate["finishReason"].as_str() {
            if !matches!(reason, "STOP" | "MAX_TOKENS") {
                return Err(invalid("Gemini generation was blocked"));
            }
            self.metadata.insert("finish_reason".into(), json!(reason));
            self.terminal = true;
        }
        Ok(events)
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

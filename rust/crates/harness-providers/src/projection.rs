use crate::{Adapter, ResolvedModel, invalid, registry::validate_url};
use acyclic_harness::{
    Result,
    context::ModelTokenCount,
    model::{
        FileProjectionPolicy, ModelContent, ModelContentPart, ModelDataPart, ModelMessage,
        ModelRequest, ModelRole, PreparedModelRequest, ToolResultContent,
    },
};
use serde_json::{Value, json};

pub(crate) fn endpoint(provider: &ResolvedModel) -> Result<String> {
    let mut base = validate_url(&provider.config.base_url)?;
    let path = format!("{}/", base.path().trim_end_matches('/'));
    base.set_path(&path);
    let suffix = match provider.config.adapter {
        Adapter::Anthropic => "messages".into(),
        Adapter::Responses => "responses".into(),
        Adapter::Chat => "chat/completions".into(),
        Adapter::Gemini => {
            let name = &provider.model.name;
            if name.contains('/') || name.contains(':') || name.contains('?') || name.contains('#')
            {
                return Err(invalid("invalid Gemini model name"));
            }
            format!("models/{name}:streamGenerateContent?alt=sse")
        }
    };
    base.join(&suffix)
        .map(String::from)
        .map_err(|_| invalid("invalid model endpoint"))
}

pub(crate) fn count(
    provider: &ResolvedModel,
    prepared: &PreparedModelRequest,
) -> Result<ModelTokenCount> {
    let request = prepared.request();
    let mut fixed = body(provider, request)?;
    let body_bytes = encoded_len(&fixed)?;
    let field = match provider.config.adapter {
        Adapter::Responses => "input",
        Adapter::Gemini => "contents",
        _ => "messages",
    };
    fixed[field] = json!([]);
    if let Some(map) = fixed.as_object_mut() {
        #[cfg(not(target_arch = "wasm32"))]
        if provider.codex_login.is_some() {
            map.remove("instructions");
        }
        match provider.config.adapter {
            Adapter::Anthropic => {
                map.remove("system");
            }
            Adapter::Gemini => {
                map.remove("systemInstruction");
            }
            _ => {}
        }
    }
    // Include protocol escaping/framing and calibrate the selected model from actual usage.
    let message_bytes = request
        .messages
        .iter()
        .map(|message| {
            encoded_len(&Value::Array(project_message(
                provider.config.adapter,
                message,
            )?))
        })
        .collect::<Result<Vec<_>>>()?;
    let total_messages: usize = message_bytes.iter().sum();
    let fixed_bytes = encoded_len(&fixed)?.max(body_bytes.saturating_sub(total_messages));
    let tokens = |bytes| {
        provider
            .estimator
            .tokens(bytes)
            .ok_or_else(|| invalid("token count exceeds u32"))
    };
    Ok(ModelTokenCount {
        request_digest: prepared.request_digest(),
        fixed_tokens: tokens(fixed_bytes)?,
        message_tokens: message_bytes
            .into_iter()
            .map(tokens)
            .collect::<Result<_>>()?,
    })
}

pub(crate) fn encoded_len(value: &Value) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("token size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, value).map_err(|error| invalid(error.to_string()))?;
    Ok(count.0)
}

pub(crate) fn body(provider: &ResolvedModel, request: &ModelRequest) -> Result<Value> {
    let adapter = provider.config.adapter;
    let mut messages = Vec::new();
    let mut system = Vec::new();
    for message in &request.messages {
        #[cfg(not(target_arch = "wasm32"))]
        if provider.codex_login.is_some() && message.role == ModelRole::System {
            system.push(text_content(&message.content)?);
            continue;
        }
        if message.role == ModelRole::System
            && matches!(adapter, Adapter::Anthropic | Adapter::Gemini)
        {
            system.push(text_content(&message.content)?);
        } else {
            for mut projected in project_message(adapter, message)? {
                // A tool batch is one model turn, followed by one result turn.
                if adapter == Adapter::Gemini
                    && messages
                        .last()
                        .is_some_and(|last: &Value| last["role"] == projected["role"])
                {
                    if let (Some(previous), Some(parts)) =
                        (messages.last_mut(), projected["parts"].as_array_mut())
                    {
                        previous["parts"]
                            .as_array_mut()
                            .ok_or_else(|| invalid("invalid projected Gemini parts"))?
                            .append(parts);
                    }
                } else if adapter == Adapter::Chat
                    && projected["role"] == "assistant"
                    && projected["tool_calls"].is_array()
                    && messages.last().is_some_and(|last| {
                        last["role"] == "assistant" && last["tool_calls"].is_array()
                    })
                {
                    let previous = messages
                        .last_mut()
                        .ok_or_else(|| invalid("missing tool batch"))?;
                    previous["tool_calls"]
                        .as_array_mut()
                        .ok_or_else(|| invalid("invalid Chat calls"))?
                        .append(
                            projected["tool_calls"]
                                .as_array_mut()
                                .ok_or_else(|| invalid("invalid Chat calls"))?,
                        );
                    merge_chat_text(&mut previous["content"], projected["content"].take())?;
                } else {
                    messages.push(projected);
                }
            }
        }
    }
    let tools: Vec<Value> = request.tools.iter().map(|tool| match adapter {
        Adapter::Anthropic => json!({"name":tool.name,"description":tool.description,"input_schema":tool.input_schema}),
        Adapter::Responses => json!({"type":"function","name":tool.name,"description":tool.description,
            "parameters":tool.input_schema,"strict":false}),
        Adapter::Chat => json!({"type":"function","function":{"name":tool.name,
            "description":tool.description,"parameters":tool.input_schema}}),
        Adapter::Gemini => json!({"name":tool.name,"description":tool.description,"parametersJsonSchema":tool.input_schema}),
    }).collect();
    let max = request
        .max_output_tokens
        .unwrap_or(provider.context_output());
    if max == 0 || max > provider.context_output() {
        return Err(invalid("requested output exceeds selected model capacity"));
    }
    let mut body = request.model.options.clone();
    let map = body
        .as_object_mut()
        .ok_or_else(|| invalid("model options must be an object"))?;
    map.insert("model".into(), json!(request.model.name));
    match adapter {
        Adapter::Chat => {
            map.insert("messages".into(), json!(messages));
            map.insert("stream".into(), json!(true));
            map.insert("stream_options".into(), json!({"include_usage":true}));
            map.insert("max_completion_tokens".into(), json!(max));
            if !tools.is_empty() {
                map.insert("tools".into(), json!(tools));
            }
        }
        Adapter::Responses => {
            let include = map
                .entry("include")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or_else(|| invalid("Responses include must be an array"))?;
            if !include
                .iter()
                .any(|item| item == "reasoning.encrypted_content")
            {
                include.push(json!("reasoning.encrypted_content"));
            }
            map.insert("input".into(), json!(messages));
            map.insert("stream".into(), json!(true));
            map.insert("store".into(), json!(false));
            map.insert("max_output_tokens".into(), json!(max));
            map.insert("tools".into(), json!(tools));
            #[cfg(not(target_arch = "wasm32"))]
            if provider.codex_login.is_some() {
                map.remove("max_output_tokens");
                map.insert("instructions".into(), json!(system.join("\n\n")));
                map.insert("tool_choice".into(), json!("auto"));
                map.insert("parallel_tool_calls".into(), json!(true));
                if map.get("reasoning").is_some_and(Value::is_null) {
                    map.remove("reasoning");
                }
            }
        }
        Adapter::Anthropic => {
            map.insert("messages".into(), json!(messages));
            map.insert("stream".into(), json!(true));
            map.insert("max_tokens".into(), json!(max));
            if !system.is_empty() {
                map.insert("system".into(), json!(system.join("\n\n")));
            }
            if !tools.is_empty() {
                map.insert("tools".into(), json!(tools));
            }
        }
        Adapter::Gemini => {
            map.remove("model");
            map.insert("contents".into(), json!(messages));
            map.entry("generationConfig")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| invalid("Gemini generationConfig must be an object"))?
                .insert("maxOutputTokens".into(), json!(max));
            if !system.is_empty() {
                map.insert(
                    "systemInstruction".into(),
                    json!({"parts":[{"text":system.join("\n\n")}]}),
                );
            }
            if !tools.is_empty() {
                map.insert("tools".into(), json!([{"functionDeclarations":tools}]));
            }
        }
    }
    Ok(body)
}

fn merge_chat_text(previous: &mut Value, next: Value) -> Result<()> {
    match (previous, next) {
        (_, Value::Null) => {}
        (previous @ Value::Null, next) => *previous = next,
        (Value::Array(previous), Value::Array(mut next)) => previous.append(&mut next),
        _ => return Err(invalid("invalid Chat tool batch content")),
    }
    Ok(())
}

fn project_message(adapter: Adapter, message: &ModelMessage) -> Result<Vec<Value>> {
    let role = match message.role {
        ModelRole::System => "system",
        ModelRole::User => "user",
        ModelRole::Assistant => "assistant",
        ModelRole::Tool => "tool",
    };
    let mut texts = Vec::new();
    let mut calls = Vec::new();
    let mut results = Vec::new();
    if let ModelContent::Text(text) = &message.content {
        texts.push(text.clone());
    }
    for part in message.content.parts() {
        match part {
            ModelContentPart::Text { text } => texts.push(text.clone()),
            ModelContentPart::File { file, policy } => texts.push(file_text(file, policy)?),
            ModelContentPart::ToolCall(acyclic_harness::model::ToolCall {
                call_id,
                name,
                arguments,
                metadata,
            }) => {
                if adapter == Adapter::Anthropic {
                    if let Some(thinking) = metadata["anthropic_thinking"].as_array() {
                        calls.extend(thinking.iter().cloned());
                    }
                }
                if adapter == Adapter::Responses {
                    if let Some(reasoning) = metadata["openai_reasoning"].as_array() {
                        calls.extend(reasoning.iter().cloned());
                    }
                }
                let mut call = match adapter {
                    Adapter::Anthropic => {
                        json!({"type":"tool_use","id":call_id,"name":name,"input":arguments})
                    }
                    Adapter::Responses => {
                        json!({"type":"function_call","call_id":call_id,"name":name,
                    "arguments":serde_json::to_string(arguments).map_err(|e| invalid(e.to_string()))?})
                    }
                    Adapter::Chat => json!({"id":call_id,"type":"function","function":{"name":name,
                    "arguments":serde_json::to_string(arguments).map_err(|e| invalid(e.to_string()))?}}),
                    Adapter::Gemini => {
                        json!({"functionCall":{"id":call_id,"name":name,"args":arguments}})
                    }
                };
                if adapter == Adapter::Gemini {
                    if let Some(signature) = metadata.get("gemini_thought_signature") {
                        call["thoughtSignature"] = signature.clone();
                    }
                }
                calls.push(call);
            }
            ModelContentPart::ToolResult {
                call_id,
                name,
                content,
            } => {
                let text = result_text(content)?;
                results.push(match adapter {
                    Adapter::Anthropic => json!({"type":"tool_result","tool_use_id":call_id,"content":text}),
                    Adapter::Responses => json!({"type":"function_call_output","call_id":call_id,"output":text}),
                    Adapter::Chat => json!({"role":"tool","tool_call_id":call_id,"content":text}),
                    Adapter::Gemini => json!({"functionResponse":{"id":call_id,"name":name,"response":{"output":text}}}),
                });
            }
        }
    }
    let mut output = Vec::new();
    match adapter {
        Adapter::Chat => {
            if !texts.is_empty() || !calls.is_empty() || results.is_empty() {
                let mut value = json!({"role":role,"content":if texts.is_empty() {Value::Null} else {json!(texts.join("\n"))}});
                if !calls.is_empty() {
                    value["tool_calls"] = json!(calls);
                }
                output.push(value);
            }
            output.extend(results);
        }
        Adapter::Responses => {
            if !texts.is_empty() {
                let kind = if message.role == ModelRole::Assistant {
                    "output_text"
                } else {
                    "input_text"
                };
                let content: Vec<_> = texts
                    .into_iter()
                    .map(|text| json!({"type":kind,"text":text}))
                    .collect();
                output.push(json!({"type":"message","role":role,"content":content}));
            }
            output.extend(calls);
            output.extend(results);
        }
        Adapter::Anthropic | Adapter::Gemini => {
            let mut parts: Vec<_> = texts
                .into_iter()
                .map(|text| {
                    if adapter == Adapter::Anthropic {
                        json!({"type":"text","text":text})
                    } else {
                        json!({"text":text})
                    }
                })
                .collect();
            parts.extend(calls);
            parts.extend(results);
            output.push(if adapter == Adapter::Anthropic {
                json!({"role":if role == "tool" {"user"} else {role},"content":parts})
            } else {
                json!({"role":if role == "assistant" {"model"} else {"user"},"parts":parts})
            });
        }
    }
    Ok(output)
}

fn result_text(content: &ToolResultContent) -> Result<String> {
    match content {
        ToolResultContent::Json { value } => {
            serde_json::to_string(value).map_err(|error| invalid(error.to_string()))
        }
        ToolResultContent::Parts { parts } => parts
            .iter()
            .map(|part| match part {
                ModelDataPart::Text { text } => Ok(text.clone()),
                ModelDataPart::File { file, policy } => file_text(file, policy),
            })
            .collect::<Result<Vec<_>>>()
            .map(|parts| parts.join("\n")),
    }
}

fn text_content(content: &ModelContent) -> Result<String> {
    if let ModelContent::Text(text) = content {
        return Ok(text.clone());
    }
    content
        .parts()
        .iter()
        .map(|part| match part {
            ModelContentPart::Text { text } => Ok(text.clone()),
            ModelContentPart::File { file, policy } => file_text(file, policy),
            _ => Err(invalid("system messages cannot contain tool exchanges")),
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("\n"))
}

fn file_text(
    file: &acyclic_harness::conversation::FileRef,
    policy: &FileProjectionPolicy,
) -> Result<String> {
    if !matches!(policy, FileProjectionPolicy::Reference) {
        return Err(invalid(
            "file bytes require an owner-mediated text projection before dispatch",
        ));
    }
    let value = serde_json::to_string(file).map_err(|e| invalid(e.to_string()))?;
    Ok(format!("[file: {value}]"))
}

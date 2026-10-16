use crate::{Adapter, ResolvedModel, invalid, registry::validate_url};
use acyclic_harness::{
    Result,
    context::ModelTokenCount,
    model::{
        FileProjectionPolicy, ModelContent, ModelContentPart, ModelDataPart, ModelMessage,
        ModelRequest, ModelRole, PreparedModelRequest, ToolResultContent,
    },
};
use base64::Engine as _;
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
    crate::images::encoded_bytes(provider, request)?;
    let mut fixed = body(provider, request)?;
    let body_bytes = encoded_len(&fixed)?;
    let field = match provider.config.adapter {
        Adapter::Responses => "input",
        Adapter::Gemini => "contents",
        _ => "messages",
    };
    fixed[field] = json!([]);
    if let Some(map) = fixed.as_object_mut() {
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
            let projected = project_message(provider, message, &mut std::iter::empty(), true)?;
            let bytes = encoded_len(&Value::Array(projected))?;
            let image_tokens =
                crate::images::files(message).try_fold(0u32, |total, (file, policy)| {
                    let amount = crate::images::support(provider, file, policy)?
                        .map_or(0, |limits| limits.maximum_tokens);
                    total
                        .checked_add(amount)
                        .ok_or_else(|| invalid("image token count overflows"))
                })?;
            let text = usize::try_from(crate::images::text_bytes(message)?)
                .map_err(|_| invalid("text projection size overflows"))?;
            let bytes = bytes
                .checked_add(text)
                .ok_or_else(|| invalid("text projection size overflows"))?;
            Ok((bytes, image_tokens))
        })
        .collect::<Result<Vec<_>>>()?;
    let total_messages: usize = message_bytes.iter().map(|(bytes, _)| bytes).sum();
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
            .map(|(bytes, images)| {
                tokens(bytes)?
                    .checked_add(images)
                    .ok_or_else(|| invalid("image token count overflows"))
            })
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
    body_with_images(provider, request, std::iter::empty(), true)
}

pub(crate) fn body_with_images<'a>(
    provider: &ResolvedModel,
    request: &ModelRequest,
    mut images: impl Iterator<Item = &'a [u8]>,
    counting: bool,
) -> Result<Value> {
    crate::images::encoded_bytes(provider, request)?;
    let adapter = provider.config.adapter;
    let mut messages = Vec::new();
    let mut system = Vec::new();
    let mut tool_images = Vec::new();
    for message in &request.messages {
        if message.role != ModelRole::Tool {
            messages.append(&mut tool_images);
        }
        if message.role == ModelRole::System
            && matches!(adapter, Adapter::Anthropic | Adapter::Gemini)
        {
            system.push(text_content(
                provider,
                &message.content,
                &mut images,
                counting,
            )?);
        } else {
            for mut projected in project_message(provider, message, &mut images, counting)? {
                if adapter == Adapter::Chat
                    && message.role == ModelRole::Tool
                    && projected["role"] == "user"
                {
                    tool_images.push(projected);
                    continue;
                }
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
    messages.append(&mut tool_images);
    if !counting && images.next().is_some() {
        return Err(invalid("resolved images differ from the model request"));
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
        (Value::String(previous), Value::String(next)) => {
            previous.push('\n');
            previous.push_str(&next);
        }
        (Value::Array(previous), Value::Array(mut next)) => previous.append(&mut next),
        (Value::Array(previous), Value::String(next)) => {
            previous.push(json!({"type":"text", "text":next}));
        }
        (previous @ Value::String(_), Value::Array(mut next)) => {
            let text = previous.take();
            next.insert(0, json!({"type":"text", "text":text}));
            *previous = Value::Array(next);
        }
        _ => return Err(invalid("invalid Chat tool batch content")),
    }
    Ok(())
}

fn project_message<'a>(
    provider: &ResolvedModel,
    message: &ModelMessage,
    images: &mut impl Iterator<Item = &'a [u8]>,
    counting: bool,
) -> Result<Vec<Value>> {
    let adapter = provider.config.adapter;
    let role = match message.role {
        ModelRole::System => "system",
        ModelRole::User => "user",
        ModelRole::Assistant => "assistant",
        ModelRole::Tool => "tool",
    };
    let mut texts = Vec::new();
    let mut content = Vec::new();
    let mut calls = Vec::new();
    let mut results = Vec::new();
    let mut extra_images = Vec::new();
    if let ModelContent::Text(text) = &message.content {
        texts.push(text.clone());
        content.push(text_part(adapter, text));
    }
    for part in message.content.parts() {
        match part {
            ModelContentPart::Text { text } => {
                texts.push(text.clone());
                content.push(text_part(adapter, text));
            }
            ModelContentPart::File { file, policy } => {
                let part = file_part(
                    provider,
                    file,
                    policy,
                    images,
                    counting,
                    message.role == ModelRole::User,
                )?;
                if let Some(text) = part["text"].as_str() {
                    texts.push(text.to_owned());
                }
                content.push(part);
            }
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
                let parts = result_parts(provider, content, images, counting)?;
                let text = parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_images = parts.iter().any(|part| !part["text"].is_string());
                results.push(match adapter {
                    Adapter::Anthropic => json!({"type":"tool_result","tool_use_id":call_id,"content":if has_images { json!(parts) } else { json!(text) }}),
                    Adapter::Responses => json!({"type":"function_call_output","call_id":call_id,"output":if has_images { json!(parts) } else { json!(text) }}),
                    Adapter::Chat => json!({"role":"tool","tool_call_id":call_id,"content":text}),
                    Adapter::Gemini => json!({"functionResponse":{"id":call_id,"name":name,"response":{"output":text}}}),
                });
                if has_images && matches!(adapter, Adapter::Chat | Adapter::Gemini) {
                    let mut captioned = vec![text_part(
                        adapter,
                        &format!("Images from tool {name} ({call_id}):"),
                    )];
                    captioned.extend(parts.into_iter().filter(|part| !part["text"].is_string()));
                    if adapter == Adapter::Chat {
                        extra_images.push(json!({"role":"user","content":captioned}));
                    } else {
                        results.extend(captioned);
                    }
                }
            }
        }
    }
    let mut output = Vec::new();
    match adapter {
        Adapter::Chat => {
            if !content.is_empty() || !calls.is_empty() || results.is_empty() {
                let value = if content.len() == texts.len() {
                    if texts.is_empty() {
                        Value::Null
                    } else {
                        json!(texts.join("\n"))
                    }
                } else {
                    json!(content)
                };
                let mut value = json!({"role":role,"content":value});
                if !calls.is_empty() {
                    value["tool_calls"] = json!(calls);
                }
                output.push(value);
            }
            output.extend(results);
            output.extend(extra_images);
        }
        Adapter::Responses => {
            if !content.is_empty() {
                let value = if content.len() == texts.len() {
                    json!(texts.join("\n"))
                } else {
                    json!(content)
                };
                output.push(json!({"role":role,"content":value}));
            }
            output.extend(calls);
            output.extend(results);
        }
        Adapter::Anthropic | Adapter::Gemini => {
            let mut parts = content;
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

fn text_part(adapter: Adapter, text: &str) -> Value {
    match adapter {
        Adapter::Gemini => json!({"text":text}),
        Adapter::Responses => json!({"type":"input_text","text":text}),
        _ => json!({"type":"text","text":text}),
    }
}

fn image_part(
    adapter: Adapter,
    file: &acyclic_harness::conversation::FileRef,
    policy: &acyclic_harness::model::NativeMediaPolicy,
    bytes: &[u8],
) -> Value {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mime = file.descriptor().media_type();
    let detail = match policy.intent {
        acyclic_harness::model::NativeMediaIntent::Image { detail } => match detail {
            acyclic_harness::model::ImageDetail::Auto => "auto",
            acyclic_harness::model::ImageDetail::Low => "low",
            acyclic_harness::model::ImageDetail::High => "high",
        },
        _ => "auto",
    };
    match adapter {
        Adapter::Anthropic => {
            json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}})
        }
        Adapter::Gemini => json!({"inlineData":{"mimeType":mime,"data":data}}),
        Adapter::Responses => {
            json!({"type":"input_image","image_url":format!("data:{mime};base64,{data}"),"detail":detail})
        }
        Adapter::Chat => {
            json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}"),"detail":detail}})
        }
    }
}

fn result_parts<'a>(
    provider: &ResolvedModel,
    content: &ToolResultContent,
    images: &mut impl Iterator<Item = &'a [u8]>,
    counting: bool,
) -> Result<Vec<Value>> {
    match content {
        ToolResultContent::Json { value } => {
            let text = serde_json::to_string(value).map_err(|error| invalid(error.to_string()))?;
            Ok(vec![text_part(provider.config.adapter, &text)])
        }
        ToolResultContent::Parts { parts } => parts
            .iter()
            .map(|part| match part {
                ModelDataPart::Text { text } => Ok(text_part(provider.config.adapter, text)),
                ModelDataPart::File { file, policy } => {
                    file_part(provider, file, policy, images, counting, true)
                }
            })
            .collect(),
    }
}

fn text_content<'a>(
    provider: &ResolvedModel,
    content: &ModelContent,
    images: &mut impl Iterator<Item = &'a [u8]>,
    counting: bool,
) -> Result<String> {
    if let ModelContent::Text(text) = content {
        return Ok(text.clone());
    }
    content
        .parts()
        .iter()
        .map(|part| match part {
            ModelContentPart::Text { text } => Ok(text.clone()),
            ModelContentPart::File { file, policy } => {
                let part = file_part(provider, file, policy, images, counting, false)?;
                part["text"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("system image was not reduced to a fallback note"))
            }
            _ => Err(invalid("system messages cannot contain tool exchanges")),
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("\n"))
}

fn file_part<'a>(
    provider: &ResolvedModel,
    file: &acyclic_harness::conversation::FileRef,
    policy: &FileProjectionPolicy,
    images: &mut impl Iterator<Item = &'a [u8]>,
    counting: bool,
    allowed: bool,
) -> Result<Value> {
    if matches!(policy, FileProjectionPolicy::BoundedFull) {
        let bytes = match images.next() {
            Some(bytes) => bytes,
            None if counting => &[],
            None => return Err(invalid("text file was not resolved before dispatch")),
        };
        let text = std::str::from_utf8(bytes).map_err(|_| invalid("text file is not UTF-8"))?;
        return Ok(text_part(provider.config.adapter, text));
    }
    if let FileProjectionPolicy::Native(policy) = policy
        && matches!(
            policy.intent,
            acyclic_harness::model::NativeMediaIntent::Image { .. }
        )
    {
        if allowed && crate::images::support(provider, file, policy)?.is_some() {
            let bytes = match images.next() {
                Some(bytes) => bytes,
                None if counting => &[],
                None => return Err(invalid("image file was not resolved before dispatch")),
            };
            return Ok(image_part(provider.config.adapter, file, policy, bytes));
        }
        let note = format!(
            "[Image {} omitted: {}/{} does not support this image format or message role]",
            file.display_name(),
            provider.model.provider,
            provider.model.name
        );
        return Ok(text_part(provider.config.adapter, &note));
    }
    Ok(text_part(
        provider.config.adapter,
        &file_text(file, policy)?,
    ))
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

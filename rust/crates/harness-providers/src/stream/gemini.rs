use super::{Call, ModelEvent, Result, State, Value, invalid, json, string, text};

pub(super) fn parse(state: &mut State, value: &Value, maximum: usize) -> Result<Vec<ModelEvent>> {
    state.usage(&value["usageMetadata"]);
    if let Some(id) = value.get("responseId") {
        state.metadata.insert("id".into(), id.clone());
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
                let index = state.calls.len() as u64;
                let arguments = function["args"].to_string();
                if arguments.len() > maximum {
                    return Err(invalid("tool arguments exceed configured limit"));
                }
                state.calls.insert(
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
        state.metadata.insert("finish_reason".into(), json!(reason));
        state.terminal = true;
    }
    Ok(events)
}

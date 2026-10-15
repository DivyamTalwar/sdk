use super::{Call, ModelEvent, Result, State, Value, append, index, invalid, string, text};

pub(super) fn parse(state: &mut State, value: &Value, maximum: usize) -> Result<Vec<ModelEvent>> {
    let mut events = Vec::new();
    match value["type"].as_str() {
        Some("message_start") => {
            state.identity(&value["message"]);
            state.usage(&value["message"]["usage"]);
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
                    if state.calls.insert(index(value, "index")?, call).is_some() {
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
                    let call = state
                        .calls
                        .get_mut(&index(value, "index")?)
                        .ok_or_else(|| invalid("tool delta without block"))?;
                    append(&mut call.arguments, delta.get("partial_json"), maximum)?;
                }
                _ => {}
            }
        }
        Some("content_block_stop") => {
            if let Some(call) = state.calls.get_mut(&index(value, "index")?) {
                if call.arguments.is_empty() {
                    call.arguments = "{}".into();
                }
            }
        }
        Some("message_delta") => {
            state.usage(&value["usage"]);
            if let Some(reason) = value["delta"].get("stop_reason") {
                state
                    .metadata
                    .insert("finish_reason".into(), reason.clone());
            }
        }
        Some("message_stop") => state.terminal = true,
        _ => {}
    }
    Ok(events)
}

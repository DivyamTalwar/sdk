use super::{ModelEvent, Result, State, Value, append, index, invalid, string, text};

pub(super) fn parse(state: &mut State, value: &Value, maximum: usize) -> Result<Vec<ModelEvent>> {
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
            if item["type"] == "reasoning" && item["encrypted_content"].is_string() {
                state
                    .thinking
                    .insert(index(value, "output_index")?, item.clone());
            }
            if item["type"] == "function_call" {
                let call = state
                    .calls
                    .entry(index(value, "output_index")?)
                    .or_default();
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
            let call = state
                .calls
                .get_mut(&index(value, "output_index")?)
                .ok_or_else(|| invalid("tool delta without item"))?;
            append(&mut call.arguments, value.get("delta"), maximum)?;
        }
        Some("response.function_call_arguments.done") => {
            let call = state
                .calls
                .get_mut(&index(value, "output_index")?)
                .ok_or_else(|| invalid("tool done without item"))?;
            call.arguments.clear();
            append(&mut call.arguments, value.get("arguments"), maximum)?;
        }
        Some("response.completed" | "response.incomplete") => {
            let response = &value["response"];
            if response["status"]
                .as_str()
                .is_some_and(|status| !matches!(status, "completed" | "incomplete"))
            {
                return Err(invalid("Responses generation did not complete"));
            }
            state.identity(response);
            state.usage(&response["usage"]);
            if value["type"] == "response.incomplete" || response["status"] == "incomplete" {
                if response["incomplete_details"]["reason"] != "max_output_tokens" {
                    return Err(invalid("Responses generation was incomplete"));
                }
                state
                    .metadata
                    .insert("finish_reason".into(), Value::String("length".into()));
            }
            state.terminal = true;
        }
        _ => {}
    }
    Ok(events)
}

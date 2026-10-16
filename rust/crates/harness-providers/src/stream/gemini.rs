use super::{ModelEvent, Result, State, Value, invalid, string, text};

pub(super) fn parse(state: &mut State, value: &Value) -> Result<Vec<ModelEvent>> {
    state.usage(&value["usageMetadata"])?;
    if let Some(id) = value.get("responseId") {
        state.budget.member(&mut state.metadata, "id", id)?;
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
            let position = (state.calls.len() + state.thinking.len()) as u64;
            if part["thought"] == true || part.get("thoughtSignature").is_some() {
                state.retain_thinking(position, part)?;
            }
            if let Some(function) = part.get("functionCall") {
                let index = (state.calls.len() + state.thinking.len()) as u64;
                let (call, budget) = state.call(index)?;
                if let Some(id) = function["id"].as_str() {
                    budget.string(&mut call.id, id)?;
                }
                budget.string(&mut call.name, string(&function["name"])?)?;
                match function.get("args") {
                    None => budget.string(&mut call.arguments, "{}")?,
                    Some(args) if args.is_object() => budget.encoded(&mut call.arguments, args)?,
                    Some(_) => return Err(invalid("tool arguments must be a JSON object")),
                }
            }
        }
    }
    if let Some(reason) = candidate["finishReason"].as_str() {
        if !matches!(reason, "STOP" | "MAX_TOKENS") {
            return Err(invalid("Gemini generation was blocked"));
        }
        state.finish_reason(if reason == "MAX_TOKENS" {
            "length"
        } else {
            reason
        })?;
        state.terminal = true;
    }
    Ok(events)
}

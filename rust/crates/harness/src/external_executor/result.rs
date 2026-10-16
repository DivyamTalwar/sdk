use crate::{Error, Outcome, Result};
use serde_json::{Value, json};

/// Projects the existing canonical tool outcome into MCP/Pi's content format.
/// Unknown or unfinished outcomes never become a successful native result.
pub fn tool_result(answer: Value) -> Result<Value> {
    match serde::Deserialize::deserialize(&answer)
        .map_err(|_| Error::Invalid("invalid harness tool outcome".into()))?
    {
        Outcome::Succeeded(value) => Ok(json!({
            "content":[{"type":"text","text":value.to_string()}],
            "structuredContent":if value.is_object() {value.clone()} else {json!({"result":value})},
            "details":value,"isError":false,
        })),
        Outcome::<Value>::Failed { message } => Err(Error::Invalid(message)),
        Outcome::Cancelled => Err(Error::InteractionRejected(
            crate::InteractionRejection::Cancelled,
        )),
        Outcome::Indeterminate { operation_id } => Err(Error::Indeterminate(operation_id)),
    }
}

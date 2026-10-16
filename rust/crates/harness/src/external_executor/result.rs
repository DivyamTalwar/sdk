use crate::{Error, Outcome, Result};
use serde_json::{Value, json};

/// Projects the existing canonical tool outcome into MCP/Pi's content format.
/// Unknown or unfinished outcomes never become a successful native result.
pub fn tool_result(answer: Value) -> Result<Value> {
    let value = value(answer)?;
    Ok(
        json!({"content":[{"type":"text","text":value.to_string()}],"details":value,"isError":false}),
    )
}

pub(super) fn value(answer: Value) -> Result<Value> {
    match serde_json::from_value(answer)
        .map_err(|_| Error::Invalid("invalid harness tool outcome".into()))?
    {
        Outcome::Succeeded(value) => Ok(value),
        Outcome::<Value>::Failed { message } => Err(Error::Invalid(message)),
        Outcome::Cancelled => Err(Error::InteractionRejected(
            crate::InteractionRejection::Cancelled,
        )),
        Outcome::Indeterminate { operation_id } => Err(Error::Indeterminate(operation_id)),
    }
}

//! Native catalog entries for the existing stock task-command contracts.
use crate::tool::ToolDefinition;
use serde_json::json;

pub(super) fn definitions() -> Vec<ToolDefinition> {
    let operation = json!({"type":"string","format":"uuid"});
    [
        ("acyclic_spawn", "Admit an exact registered child task", json!({
            "name":{"type":"string"},"version":{"type":"string"},"input":{}
        }), vec!["name", "version", "input"]),
        ("acyclic_send", "Send a pinned readable file to an admitted task", json!({
            "recipient":operation,"payload":{"type":"object"}
        }), vec!["recipient", "payload"]),
        ("acyclic_steer", "Send a pinned steering file to an admitted task's mailbox", json!({
            "recipient":operation,"payload":{"type":"object"}
        }), vec!["recipient", "payload"]),
        ("acyclic_receive", "Receive the first mailbox file after the acknowledged sequence", json!({
            "after":{"type":"integer","minimum":0}
        }), vec!["after"]),
        ("acyclic_wait", "Wait for a direct child admitted by an earlier command", json!({
            "admission_operation":operation
        }), vec!["admission_operation"]),
        ("acyclic_cancel", "Cancel a direct child and its subtree under the caller's fence", json!({
            "child":operation
        }), vec!["child"]),
    ].into_iter().map(|(name, description, properties, required)| {
        let output = json!({});
        ToolDefinition {
            name:name.into(), revision:"1".into(), description:description.into(),
            input_schema:json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
            output_schema:output.clone(), projection_schema:crate::tool::json_projection_schema(output),
        }
    }).collect()
}

pub(super) fn catalog() -> crate::Result<Vec<ToolDefinition>> {
    let mut tools = crate::tools::defaults()?.definitions(None)?;
    tools.extend(definitions());
    Ok(tools)
}

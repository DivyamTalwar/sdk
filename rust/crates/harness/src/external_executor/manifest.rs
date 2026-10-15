use super::{ExternalHost, HookAction, hooks};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Generates Claude's command hooks from the shared executor catalog.
pub fn claude_hooks(executable: &str) -> BTreeMap<&'static str, Vec<Value>> {
    hooks(ExternalHost::ClaudeCode)
        .map(|spec| {
            let mut group = json!({"hooks": [{
                "type": "command",
                "command": executable,
                "args": ["__hook", "claude-code", spec.event],
                "timeout": spec.timeout_seconds(),
                "statusMessage": "Acyclic is routing the workspace"
            }]});
            if matches!(spec.action, HookAction::RunTool | HookAction::ToolFinished) {
                group["matcher"] = json!(".*");
            }
            (spec.event, vec![group])
        })
        .collect()
}

/// Generates the Claude plugin manifest shipped alongside the other host assets.
#[must_use]
pub fn claude_manifest(version: &str) -> Value {
    json!({
        "name": "acyclic",
        "version": version,
        "description": "Run tools and fork subagents through the shared Acyclic harness service.",
        "author": {"name":"Acyclic Labs"},
        "license": "Apache-2.0",
        "hooks": claude_hooks("${CLAUDE_PLUGIN_ROOT}/../bin/acyclic")
    })
}

use super::{ExternalHost, HookAction, hooks};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Generates Claude's command hooks from the shared executor catalog.
pub fn claude_hooks(executable: &str) -> BTreeMap<&'static str, Vec<Value>> {
    hooks(ExternalHost::ClaudeCode)
        .map(|spec| {
            let mut group = Map::new();
            group.insert(
                "hooks".into(),
                json!([{
                    "type": "command",
                    "command": executable,
                    "args": ["__hook", "claude-code", spec.event],
                    "timeout": spec.timeout_seconds(),
                    "statusMessage": "Acyclic is routing the workspace"
                }]),
            );
            if matches!(spec.action, HookAction::RunTool | HookAction::ToolFinished) {
                group.insert("matcher".into(), json!(".*"));
            }
            (spec.event, vec![Value::Object(group)])
        })
        .collect()
}

/// Generates the Claude plugin manifest shipped alongside the other host assets.
#[must_use]
pub fn claude_manifest(version: &str) -> Value {
    json!({
        "name": "acyclic",
        "version": version,
        "description": "Run tools and fork subagents through Acyclic harness actors.",
        "author": {"name":"Acyclic Labs"},
        "license": "Apache-2.0",
        "mcpServers": "./.mcp.json",
        "hooks": claude_hooks("${CLAUDE_PLUGIN_ROOT}/../bin/acyclic")
    })
}

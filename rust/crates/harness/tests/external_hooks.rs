//! Native host hook fixtures over the shared external-executor request API.
use acyclic_harness::external_executor::*;
use acyclic_harness::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Deserialize)]
struct Scenario {
    host: ExternalHost,
    event: String,
    input: Value,
    action: Value,
}

#[test]
fn host_events_create_the_same_task_request_actions() -> Result<()> {
    let scenarios: Vec<Scenario> = serde_json::from_str(include_str!("external_hooks.json"))
        .map_err(|error| Error::Invalid(error.to_string()))?;
    let mut operations = HashSet::new();
    for scenario in scenarios {
        let actual = request(scenario.host, &scenario.event, scenario.input)?;
        assert_eq!(actual.session_id, "session");
        assert_eq!(
            serde_json::to_value(&actual.action).map_err(invalid)?,
            scenario.action
        );
        assert!(
            operations.insert(actual.operation_id),
            "hook actions must have distinct identities"
        );
        let bytes = serde_json::to_vec(&actual).map_err(invalid)?;
        assert_eq!(
            serde_json::from_slice::<ExternalRequest>(&bytes).map_err(invalid)?,
            actual
        );
    }
    Ok(())
}

#[test]
fn retry_identity_does_not_turn_changed_arguments_into_another_operation() -> Result<()> {
    let input = json!({"session_id":"session","tool_use_id":"call","tool_name":"Bash","tool_input":{"command":"pwd"}});
    let original = request(ExternalHost::ClaudeCode, "PreToolUse", input.clone())?;
    assert_eq!(
        request(ExternalHost::ClaudeCode, "PreToolUse", input.clone())?,
        original
    );
    let mut changed = input.clone();
    changed["tool_input"]["command"] = json!("ls");
    let changed = request(ExternalHost::ClaudeCode, "PreToolUse", changed)?;
    assert_eq!(changed.operation_id, original.operation_id);
    assert_ne!(changed.action, original.action);
    let mut other_session = input.clone();
    other_session["session_id"] = json!("other");
    assert_ne!(
        request(ExternalHost::ClaudeCode, "PreToolUse", other_session)?.operation_id,
        original.operation_id
    );
    let mut child = input;
    child["agent_id"] = json!("child");
    assert_ne!(
        request(ExternalHost::ClaudeCode, "PreToolUse", child)?.operation_id,
        original.operation_id
    );
    Ok(())
}

#[test]
fn malformed_hooks_never_acquire_a_request_identity() {
    for (event, input) in [
        ("SessionStart", Value::Null),
        ("SessionStart", json!({"cwd":"/project"})),
        ("SessionStart", json!({"session_id":"","cwd":"/project"})),
        (
            "SessionStart",
            json!({"session_id":"session","sessionId":"other","cwd":"/project"}),
        ),
        (
            "PreToolUse",
            json!({"session_id":"session","tool_name":"Bash","tool_input":{}}),
        ),
        (
            "PreToolUse",
            json!({"session_id":"session","tool_name":"Bash","tool_use_id":"call"}),
        ),
        (
            "PostToolUse",
            json!({"session_id":"session","tool_use_id":"call","isError":"false"}),
        ),
        (
            "PostToolUseFailure",
            json!({"session_id":"session","tool_use_id":"call","isError":"false"}),
        ),
        ("SubagentStart", json!({"session_id":"session"})),
        (
            "SubagentStop",
            json!({"session_id":"session","agent_id":"\0"}),
        ),
        ("unknown", json!({"session_id":"session"})),
    ] {
        assert!(
            request(ExternalHost::ClaudeCode, event, input).is_err(),
            "{event}"
        );
    }
}

#[test]
fn native_metadata_and_credentials_are_not_copied_into_requests() -> Result<()> {
    let actual = request(
        ExternalHost::Codex,
        "SessionStart",
        json!({
            "session_id":"session", "cwd":"/project", "environment":{"API_KEY":"private-secret"},
            "transcript_path":"/private/transcript", "model":"native-model"
        }),
    )?;
    let encoded = serde_json::to_string(&actual).map_err(invalid)?;
    for private in [
        "private-secret",
        "transcript",
        "native-model",
        "environment",
    ] {
        assert!(!encoded.contains(private));
    }
    Ok(())
}

#[test]
fn claude_manifest_drives_every_catalog_hook_without_codex_mcp() -> Result<()> {
    let manifest: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../plugin/claude/.claude-plugin/plugin.json"
    )))
    .map_err(invalid)?;
    let configured = manifest["hooks"]
        .as_object()
        .ok_or_else(|| Error::Invalid("Claude hooks missing".into()))?;
    assert_eq!(manifest, claude_manifest(env!("CARGO_PKG_VERSION")));
    assert_eq!(configured.len(), hooks(ExternalHost::ClaudeCode).count());
    for spec in hooks(ExternalHost::ClaudeCode) {
        let handler = &configured[spec.event][0]["hooks"][0];
        assert_eq!(handler["type"], "command");
        assert_eq!(handler["command"], "${CLAUDE_PLUGIN_ROOT}/../bin/acyclic");
        assert_eq!(
            handler["args"],
            json!(["__hook", "claude-code", spec.event])
        );
        assert_eq!(
            handler["timeout"],
            if spec.action == HookAction::Detach {
                3
            } else {
                120
            }
        );
        if matches!(spec.action, HookAction::RunTool | HookAction::ToolFinished) {
            assert_eq!(configured[spec.event][0]["matcher"], ".*");
        }
    }
    assert!(manifest.get("mcpServers").is_none());
    Ok(())
}

fn invalid(error: serde_json::Error) -> Error {
    Error::Invalid(error.to_string())
}

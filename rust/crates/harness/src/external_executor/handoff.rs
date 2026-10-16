//! Native hooks identify a request; the MCP client returns its durable result.
use super::{ExternalAction, ExternalHost, ExternalRequest, volume};
use crate::task_volume::{Component, FileKey, FileValue, Query, TaskFile, TaskVolume};
use crate::{Error, Result};
use acyclic_stream::StreamProvider;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const FIELD: &str = "_acyclic";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    request: FileKey,
    cwd: PathBuf,
}

pub(super) fn normalize_tool(host: ExternalHost, event: &str, input: &mut Value) -> Result<bool> {
    if host == ExternalHost::Pi || event != "PreToolUse" {
        return Ok(false);
    }
    let Some(name) = input
        .get("tool_name")
        .and_then(Value::as_str)
        .and_then(|name| {
            name.strip_prefix("mcp__acyclic__")
                .or_else(|| name.strip_prefix("mcp__plugin_acyclic_acyclic__"))
        })
    else {
        return Ok(false);
    };
    let name = name.to_owned();
    let object = input.as_object_mut().ok_or_else(invalid)?;
    object.insert("tool_name".into(), Value::String(name));
    if let Some(arguments) = object.get_mut("tool_input").and_then(Value::as_object_mut) {
        arguments.remove(FIELD);
    }
    Ok(true)
}

pub(super) fn answer(
    request: &ExternalRequest,
    cwd: &Path,
    task: Component,
    result: &Value,
) -> Result<Value> {
    // A failed harness dispatch blocks the native call instead of admitting an
    // MCP execution with no canonical result.
    if !result
        .as_object()
        .is_some_and(|object| object.len() == 1 && object.contains_key("Succeeded"))
    {
        return Err(Error::Invalid(
            result
                .pointer("/Failed/message")
                .and_then(Value::as_str)
                .unwrap_or("harness tool has no successful result")
                .into(),
        ));
    }
    let ExternalAction::RunTool { arguments, .. } = &request.action else {
        return Err(invalid());
    };
    let mut arguments = arguments.as_object().cloned().ok_or_else(invalid)?;
    arguments.insert(
        FIELD.into(),
        serde_json::to_value(Handoff {
            request: request.request_key(task).map_err(|_| invalid())?,
            cwd: cwd.to_owned(),
        })
        .map_err(|error| Error::Invalid(error.to_string()))?,
    );
    Ok(
        json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":arguments}}),
    )
}

/// Checks a native handoff against its original immutable request file.
/// The result client can only follow that request's result, never execute it.
pub fn native_tool_request(
    name: &str,
    arguments: Value,
    file: &FileValue,
) -> Result<(ExternalRequest, PathBuf)> {
    let Value::Object(mut arguments) = arguments else {
        return Err(invalid());
    };
    let handoff = decode(arguments.remove(FIELD).ok_or_else(invalid)?)?;
    let FileKey::Task {
        task,
        file: TaskFile::Request(_),
    } = &handoff.request
    else {
        return Err(invalid());
    };
    if file.key != handoff.request {
        return Err(invalid());
    }
    let request = ExternalRequest::from_file(task, file).map_err(|_| invalid())?;
    let ExternalAction::RunTool {
        name: original,
        arguments: input,
        ..
    } = &request.action
    else {
        return Err(invalid());
    };
    if original != name || *input != Value::Object(arguments) {
        return Err(invalid());
    }
    Ok((request, handoff.cwd))
}

fn decode(value: Value) -> Result<Handoff> {
    volume::decode(&volume::encode(&value).map_err(|_| invalid())?).map_err(|_| invalid())
}

pub(super) fn cwd(arguments: &Value) -> Result<PathBuf> {
    decode(arguments.get(FIELD).cloned().ok_or_else(invalid)?).map(|handoff| handoff.cwd)
}

pub(crate) async fn read_result<P: StreamProvider>(
    name: &str,
    arguments: Value,
    requests: &TaskVolume<P>,
    results: &TaskVolume<P>,
) -> Result<Value> {
    let handoff = decode(arguments.get(FIELD).cloned().ok_or_else(invalid)?)?;
    let source = requests
        .snapshot(&Query::File(handoff.request), 1)
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;
    let file = source.files.first().ok_or_else(invalid)?;
    let (request, _) = native_tool_request(name, arguments, file)?;
    let FileKey::Task { task, .. } = &file.key else {
        return Err(invalid());
    };
    super::tool_result(
        request
            .follow_result(results, task.clone())
            .await
            .map_err(|e| Error::Storage(e.to_string()))?,
    )
}

pub(super) fn schema(mut schema: Value) -> Value {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert(
            FIELD.into(),
            json!({"type":"object","description":"Filled by the native hook; omit this field."}),
        );
    }
    schema
}

fn invalid() -> Error {
    Error::Unauthorized("MCP arguments differ from their native request".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn near_record_limit_tool_uses_a_constant_size_file_reference()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let request = super::super::request(
            ExternalHost::Codex,
            "PreToolUse",
            json!({
                "session_id":"boundary","tool_use_id":"call","tool_name":"bash",
                "tool_input":{"command":"x".repeat(acyclic_stream::MAX_RECORD_BYTES-2048)},
            }),
        )?;
        let task = Component::new(request.task_id().to_string())?;
        let file = request.to_file(task.clone())?;
        assert!(file.bytes.len() > acyclic_stream::MAX_RECORD_BYTES - 2048);
        let answer = answer(
            &request,
            Path::new("project"),
            task,
            &json!({"Succeeded":{}}),
        )?;
        let arguments = answer["hookSpecificOutput"]["updatedInput"].clone();
        assert!(serde_json::to_vec(&arguments[FIELD])?.len() < 1024);
        assert_eq!(native_tool_request("bash", arguments, &file)?.0, request);
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        // Property: a handoff preserves the native request byte identity;
        // changing the outer tool name/arguments cannot change its effect.
        #[test]
        fn handoff_round_trip_binds_every_argument(session in "[a-z]{1,16}", command in ".{1,64}") {
            for host in [ExternalHost::Codex,ExternalHost::ClaudeCode] {
                let request = super::super::request(host,"PreToolUse",json!({
                    "session_id":session,"tool_use_id":"call","tool_name":"bash","tool_input":{"command":command},
                }))?;
                let result = json!({"Succeeded":{"stdout":"result"}});
                let file = request.to_file(Component::new(request.task_id().to_string())?)?;
                let answer = answer(&request, Path::new("project"), Component::new(request.task_id().to_string())?, &result)?;
                let arguments = answer["hookSpecificOutput"]["updatedInput"].clone();
                let (decoded,cwd) = native_tool_request("bash",arguments.clone(),&file)?;
                prop_assert_eq!(decoded,request.clone());
                prop_assert_eq!(cwd,PathBuf::from("project"));
                prop_assert!(native_tool_request("write",arguments.clone(),&file).is_err());
                let mut changed = arguments;
                changed["command"] = json!({"different":true});
                prop_assert!(native_tool_request("bash",changed,&file).is_err());
            }
        }
    }
}

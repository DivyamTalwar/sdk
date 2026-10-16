//! The default agent tool translates calls into task-volume files and live queries.
use crate::{
    Error, Result,
    runtime::{RuntimeScope, ToolContext},
    task_volume::{
        ChildFile, ChildRequest, ChildState, Component, Control, FileKey, FileValue, Query,
        TaskFiles,
    },
    tool::{JsonToolProjection, Tool, ToolDefinition, ToolExecutor, ToolInvocation, ToolResult},
};
use acyclic_stream::{BoxProviderFuture, StreamProvider};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// File operations accepted by the default agent tool.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "command", rename_all = "lowercase", deny_unknown_fields)]
pub enum AgentCommand {
    /// Publish an immutable child request.
    Spawn {
        /// Reference scoped to the calling parent's private volume.
        reference: Component,
        /// Markdown child prompt.
        prompt: String,
        /// Optional model name within the configured provider.
        model: Option<String>,
        /// Optional attenuated tool selection.
        tools: Option<Vec<String>>,
    },
    /// Add one immutable inbox message.
    Send {
        /// Child reference.
        reference: Component,
        /// Message body.
        message: String,
    },
    /// Follow the child's status until its current work settles.
    Wait {
        /// Child reference.
        reference: Component,
    },
    /// Read the child's status.
    Status {
        /// Child reference.
        reference: Component,
    },
    /// Publish a lifecycle intent.
    Control {
        /// Child reference.
        reference: Component,
        /// Lifecycle action.
        action: Control,
    },
    /// Read active child references and their status.
    List,
}

pub(super) fn tool() -> Tool {
    let output = json!({"type":"object"});
    Tool {
        hook_names: &[],
        definition: ToolDefinition {
            name: "agent".into(),
            revision: "1".into(),
            description: "Spawn, message, wait for, inspect and control child agents through agents/<reference>/ files".into(),
            input_schema: json!({
                "type":"object", "required":["command"],
                "properties":{
                    "command":{"type":"string","enum":["spawn","send","wait","status","control","list"]},
                    "reference":{"type":"string","minLength":1},
                    "prompt":{"type":"string","minLength":1},
                    "message":{"type":"string","minLength":1},
                    "model":{"type":"string","minLength":1},
                    "tools":{"type":"array","items":{"type":"string","minLength":1}},
                    "action":{"type":"string","enum":["pause","resume","interrupt","cancel","archive"]}
                }, "additionalProperties":false
            }),
            output_schema: output.clone(),
            projection_schema: crate::tool::json_projection_schema(output),
        },
        executor: Arc::new(AgentExecutor),
        projection: Arc::new(JsonToolProjection),
    }
}

struct AgentExecutor;

impl ToolExecutor for AgentExecutor {
    fn authorize(
        &self,
        _: Option<&RuntimeScope>,
        context: Option<&ToolContext>,
        invocation: &ToolInvocation,
        work: crate::NativeWork<'_>,
    ) -> Result<()> {
        #[cfg(all(feature = "filesystem", not(target_arch = "wasm32")))]
        if work.is_some() {
            return Err(Error::NativeBoundary {
                code: crate::wire::ErrorCode::Unsupported,
                reason: "agent files require ordinary task execution",
            });
        }
        let _ = work;
        let context =
            context.ok_or_else(|| Error::Unauthorized("agent requires its task context".into()))?;
        if invocation.name != "agent"
            || context.operation_id() != invocation.operation_id
            || context.call_id() != invocation.call_id
            || !context
                .effective_scope()
                .grants()
                .contains("tool:call:agent")
        {
            return Err(Error::Unauthorized(
                "agent call differs from its task authority".into(),
            ));
        }
        if context.task().harness.task_files.is_none() {
            return Err(Error::Unsupported("agent task files are not bound".into()));
        }
        Ok(())
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            self.authorize(None, context.as_ref(), &invocation, None)?;
            let context =
                context.ok_or_else(|| Error::Unauthorized("agent context is absent".into()))?;
            let files = context
                .task()
                .harness
                .task_files
                .as_ref()
                .ok_or_else(|| Error::Unsupported("agent files are absent".into()))?;
            let command: AgentCommand =
                serde_json::from_value(invocation.arguments).map_err(invalid)?;
            access(
                files,
                command,
                invocation.operation_id,
                context.effective_scope().limits().attachments,
                false,
            )
            .await
            .and_then(|value| {
                value
                    .map(|value| ToolResult { value })
                    .ok_or(Error::Indeterminate(invocation.operation_id))
            })
        })
    }

    fn reconcile<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Option<ToolResult>>> {
        Box::pin(async move {
            self.authorize(None, context.as_ref(), &invocation, None)?;
            let context =
                context.ok_or_else(|| Error::Unauthorized("agent context is absent".into()))?;
            let files = context
                .task()
                .harness
                .task_files
                .as_ref()
                .ok_or_else(|| Error::Unsupported("agent files are absent".into()))?;
            let command = serde_json::from_value(invocation.arguments).map_err(invalid)?;
            access(
                files,
                command,
                invocation.operation_id,
                context.effective_scope().limits().attachments,
                true,
            )
            .await
            .map(|result| result.map(|value| ToolResult { value }))
        })
    }
}

async fn access<P: StreamProvider + ?Sized>(
    files: &TaskFiles<P>,
    command: AgentCommand,
    operation: crate::OperationId,
    maximum: usize,
    reconcile_only: bool,
) -> Result<Option<Value>> {
    let (reference, write, wait) = match command {
        AgentCommand::List => {
            let snapshot = files
                .observe(Query::Children, None, maximum)
                .await
                .map_err(storage)?
                .snapshot
                .ok_or_else(|| Error::Conflict("child snapshot is absent".into()))?;
            let mut children = BTreeMap::<_, Vec<_>>::new();
            for file in snapshot.files {
                if let FileKey::Child { reference, .. } = &file.key {
                    children.entry(reference.clone()).or_default().push(file);
                }
            }
            let mut active = Vec::new();
            for (reference, files) in children {
                let status = status(&reference, &files)?;
                if status.get("state") != Some(&json!(ChildState::Archived)) {
                    active.push(status);
                }
            }
            return Ok(Some(json!({"agents":active})));
        }
        AgentCommand::Spawn {
            reference,
            prompt,
            model,
            tools,
        } => {
            if prompt.is_empty() {
                return Err(invalid("child prompt is empty"));
            }
            let request = ChildRequest {
                prompt,
                model,
                tools,
            };
            let bytes = request.encode();
            ChildRequest::parse(&bytes, bytes.len() as u64)?;
            (reference, Some((ChildFile::Request, bytes)), false)
        }

        AgentCommand::Send { reference, message } => {
            if message.is_empty() {
                return Err(invalid("child message is empty"));
            }
            (
                reference,
                Some((
                    ChildFile::Inbox(Component::new(operation.to_string()).map_err(storage)?),
                    message.into_bytes(),
                )),
                false,
            )
        }
        AgentCommand::Control { reference, action } => (
            reference,
            Some((ChildFile::Control, action.bytes().to_vec())),
            false,
        ),
        AgentCommand::Status { reference } => (reference, None, false),
        AgentCommand::Wait { reference } => (reference, None, true),
    };
    let mut observation = files
        .observe(Query::Child(reference.clone()), None, maximum)
        .await
        .map_err(storage)?;
    let snapshot = observation
        .snapshot
        .take()
        .ok_or_else(|| Error::Conflict("child snapshot is absent".into()))?;
    let mut values = snapshot.files;
    let request_exists = values.iter().any(|file| {
        matches!(
            file.key,
            FileKey::Child {
                file: ChildFile::Request,
                ..
            }
        )
    });
    if !request_exists && !matches!(write, Some((ChildFile::Request, _))) {
        return Err(Error::NotFound(format!("agents/{}", reference.as_str())));
    }
    if let Some((file, bytes)) = write {
        let file = FileValue::new(
            FileKey::Child {
                reference: reference.clone(),
                file,
            },
            bytes,
        );
        if reconcile_only {
            // A newer lifecycle write must never be overwritten during replay.
            // Immutable request/inbox identities can be recognized exactly.
            if !values.contains(&file) {
                return Ok(None);
            }
        } else {
            files
                .write(snapshot.generation, &file)
                .await
                .map_err(storage)?;
        }
        values.retain(|previous| previous.key != file.key);
        values.push(file);
    }
    loop {
        let result = status(&reference, &values)?;
        if !wait || result.get("settled") == Some(&Value::Bool(true)) {
            return Ok(Some(result));
        }
        let change = observation
            .changes
            .next()
            .await
            .ok_or_else(|| Error::Conflict("child follow ended".into()))?
            .map_err(storage)?;
        if let Some(file) = change.file {
            values.retain(|previous| previous.key != file.key);
            values.push(file);
        }
    }
}

fn status(reference: &Component, files: &[FileValue]) -> Result<Value> {
    let mut state = ChildState::Running;
    let mut execution = Value::Null;
    for file in files {
        match &file.key {
            FileKey::Child {
                file: ChildFile::Control,
                ..
            } => {
                state = state.apply(Control::parse(&file.bytes).map_err(storage)?);
            }
            FileKey::Child {
                file: ChildFile::Status,
                ..
            } => {
                execution = serde_json::from_slice(&file.bytes).map_err(storage)?;
            }
            _ => {}
        }
    }
    let settled = matches!(
        state,
        ChildState::Cancelled | ChildState::Archived | ChildState::Interrupted
    ) || execution.get("settled") == Some(&Value::Bool(true));
    Ok(json!({"reference":reference,"state":state,"settled":settled,"execution":execution}))
}

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Invalid(error.to_string())
}
fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

#[cfg(all(test, not(acyclic_harness_loom)))]
#[path = "agent/tests.rs"]
mod tests;

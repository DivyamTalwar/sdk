use super::ExternalHost;
use crate::OperationId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Immutable hook request; authentication accompanies its submission separately.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRequest {
    /// Original operation identity, retained across hook retries.
    pub operation_id: OperationId,
    /// Native executor implementation.
    pub host: ExternalHost,
    /// Native session whose attachment resolves the task.
    pub session_id: String,
    /// Native turn binding, when supplied by the host.
    pub turn_id: Option<String>,
    /// Native parent/caller binding, when supplied by the host.
    pub caller: Option<String>,
    /// Requested harness operation.
    pub action: ExternalAction,
}

pub(super) fn valid_text(text: &str) -> bool {
    !text.is_empty() && !text.contains('\0')
}

impl ExternalRequest {
    pub(super) fn valid(&self) -> bool {
        let action = match &self.action {
            ExternalAction::Attach { workspace } => valid_text(workspace),
            ExternalAction::RunTool { call_id, name, .. } => {
                valid_text(call_id) && valid_text(name)
            }
            ExternalAction::ToolFinished { call_id, .. } => valid_text(call_id),
            ExternalAction::Fork { child_id, kind } => {
                valid_text(child_id) && kind.as_deref().is_none_or(valid_text)
            }
            ExternalAction::Join { child_id } => valid_text(child_id),
            ExternalAction::Detach => true,
        };
        action
            && valid_text(&self.session_id)
            && self.turn_id.as_deref().is_none_or(valid_text)
            && self.caller.as_deref().is_none_or(valid_text)
            && self.operation_id
                == super::normalize::operation(
                    self.host,
                    &self.session_id,
                    &self.caller,
                    &self.action,
                )
    }
}

/// Requests consumed by the shared task owner, never by a plugin state machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalAction {
    /// Attach a native executor to a task for this workspace.
    Attach {
        /// Native workspace path resolved through the actor's volume binding.
        workspace: String,
    },
    /// Execute one original tool call.
    RunTool {
        /// Native tool invocation identity.
        call_id: String,
        /// Native tool name, interpreted by the host tool catalog.
        name: String,
        /// Complete arguments after the native call has been assembled.
        arguments: Value,
    },
    /// Record the native tool completion.
    ToolFinished {
        /// Original tool invocation identity.
        call_id: String,
        /// Native host reported failure.
        failed: bool,
        /// Native result, if included in the hook protocol.
        result: Option<Value>,
    },
    /// Fork and attach a native child executor.
    Fork {
        /// Native child identity.
        child_id: String,
        /// Native child kind, when supplied.
        kind: Option<String>,
    },
    /// Join the previously attached child task.
    Join {
        /// Original native child identity.
        child_id: String,
    },
    /// Release this executor's attachment.
    Detach,
}

//! Native names select existing workflow commands; they introduce no executor.
use super::{ExternalAction, ExternalRequest};
use crate::{Error, Result, filesystem::*};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

impl ExternalRequest {
    pub(super) fn task_command(&self) -> Result<Option<(&'static str, Value)>> {
        let ExternalAction::RunTool {
            name, arguments, ..
        } = &self.action
        else {
            return Ok(None);
        };
        let (kind, payload) = match name.as_str() {
            "acyclic_spawn" => (
                TASK_ADMIT_COMMAND_KIND,
                canonical::<TaskAdmitCommand>(arguments)?,
            ),
            "acyclic_send" | "acyclic_steer" => (
                MAIL_SEND_TASK_COMMAND_KIND,
                canonical::<MailSendTaskCommand>(arguments)?,
            ),
            "acyclic_receive" => (
                MAIL_RECEIVE_TASK_COMMAND_KIND,
                canonical::<MailReceiveTaskCommand>(arguments)?,
            ),
            "acyclic_wait" => (
                TASK_OBSERVE_COMMAND_KIND,
                canonical::<TaskObserveCommand>(arguments)?,
            ),
            "acyclic_cancel" => (
                TASK_CANCEL_COMMAND_KIND,
                canonical::<TaskCancelCommand>(arguments)?,
            ),
            _ => return Ok(None),
        };
        Ok(Some((kind, payload)))
    }
}

fn canonical<T: DeserializeOwned + Serialize>(value: &Value) -> Result<Value> {
    let decoded = T::deserialize(value).map_err(|error| Error::Invalid(error.to_string()))?;
    serde_json::to_value(decoded).map_err(|error| Error::Invalid(error.to_string()))
}

#[cfg(feature = "test-support")]
pub(crate) fn fuzz_command(request: &ExternalRequest) {
    if let Ok(Some((kind, payload))) = request.task_command() {
        let mut replay = request.clone();
        if let ExternalAction::RunTool { arguments, .. } = &mut replay.action {
            *arguments = payload.clone();
        }
        assert_eq!(replay.task_command().unwrap(), Some((kind, payload)));
    }
}

#[cfg(all(test, not(acyclic_harness_loom)))]
mod tests;

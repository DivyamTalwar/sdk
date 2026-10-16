//! Local execution owner for the existing immutable command request files.
use super::{CommandResolution, CommandSession};
use crate::{Admission, Error, OperationId, Result, local::LocalHarness, runtime::RuntimeTask};
use acyclic_stream::LocalStream;
use serde_json::Value;
use std::sync::Arc;

/// Consume one observed request through the same claim/outcome publication as
/// hosted commands. Retrying its operation uses the stock durable admission.
pub async fn consume_local_command(
    session: &mut CommandSession<LocalStream>,
    local: &LocalHarness,
    request: &crate::task_volume::FileValue,
) -> Result<Option<Value>> {
    session
        .consume(request, |operation, target, resolution| async move {
            if target.is_some_and(|task| task.to_string() != local.client_owner().id) {
                return Err(Error::Unauthorized(
                    "command selects another local owner".into(),
                ));
            }
            match resolution {
                CommandResolution::Prompt { text, mentions, .. } => {
                    if !mentions.is_empty() {
                        return Err(Error::Unsupported(
                            "TODO #617: pinned prompt context binding".into(),
                        ));
                    }
                    let task = match local.submit(operation, &text).await? {
                        Admission::Accepted(RuntimeTask::Durable { task_id, .. }) => task_id,
                        Admission::Accepted(RuntimeTask::Live(_)) => {
                            return Err(Error::Conflict(
                                "local command requires durable admission".into(),
                            ));
                        }
                        Admission::Indeterminate { .. } => {
                            return Err(Error::Indeterminate(operation));
                        }
                        Admission::Rejected { reason } => return Err(Error::Conflict(reason)),
                    };
                    let output = local.resume(task).await?;
                    serde_json::to_value(output).map_err(|error| Error::Invalid(error.to_string()))
                }
                // TODO(#617): replace this prompt-only dispatch with the merged
                // LocalHarness::dispatch_configured_command binding.
                _ => Err(Error::Unsupported(
                    "TODO #617: local command dispatch".into(),
                )),
            }
        })
        .await
}

/// Follows ordinary command files and drives their durable local owner.
/// Poll this future alongside input/follows; dropping it never resends a claim.
pub async fn run_local_commands(local: Arc<LocalHarness>, session: OperationId) -> Result<()> {
    let mut commands = CommandSession::open(
        Arc::clone(&local.task_files().inner),
        uuid::Uuid::from_bytes(session.into_bytes()),
        65_536,
        4096,
    )
    .await?;
    loop {
        let request = commands.next_request().await?;
        match consume_local_command(&mut commands, &local, &request).await {
            // Claim CAS contention has already followed the winning cut and
            // requeued the request. Completed outcomes are never dispatched again.
            Err(Error::Conflict(_)) => continue,
            result => {
                result?;
            }
        }
    }
}

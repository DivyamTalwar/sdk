//! Local bindings shared by native clients; presentation never owns a host.
use super::{ClientStateStore, FileClientState, HarnessClient};
use crate::{Error, OperationId, Result, local::LocalHarness, wire};
use std::sync::Arc;

/// Opens the local client over the same task-volume request and follow binding
/// used by hosted clients, with private durable client state.
pub async fn connect_local(
    _local: Arc<LocalHarness>,
    _operation: OperationId,
) -> Result<(HarnessClient<FileClientState>, wire::ObserveRequest)> {
    // TODO(#495): expose the LocalHarness task-volume HarnessWireApi binding,
    // owner/control scope, initial cursor, private client-state path and file reader.
    Err(Error::Unsupported(
        "TODO #495: LocalHarness client binding is not exposed".into(),
    ))
}

/// Stages a prompt as a request file and returns its exact wire command identity.
pub async fn local_prompt(
    _local: &LocalHarness,
    _operation: OperationId,
    _prompt: &str,
) -> Result<wire::CommandEnvelope> {
    // TODO(#495): expose prompt request-file preparation without admitting or
    // executing it, so the existing client outbox remains the only submit path.
    Err(Error::Unsupported(
        "TODO #495: prompt request-file binding is not exposed".into(),
    ))
}

/// Applies a UI action by writing its task/private/config-volume files.
#[cfg(feature = "tui")]
pub async fn local_input<S: ClientStateStore>(
    local: &LocalHarness,
    client: &mut HarnessClient<S>,
    effect: crate::tui::InputEffect,
) -> Result<()> {
    if let crate::tui::InputEffect::Submit { text, .. } = effect {
        if text.starts_with('/') {
            // TODO(#617): bind the file-driven command dispatcher, including unknown-command rejection.
            return Err(Error::Unsupported(
                "TODO #617: local command-file dispatch binding".into(),
            ));
        }
        let command = local_prompt(local, OperationId::new(), &text).await?;
        return client.submit(command, |_| Ok(())).await;
    }
    // TODO(#495): expose private/config-volume UI writes for drafts, history,
    // scroll/layout and clipboard/external-editor request files.
    Err(Error::Unsupported(
        "TODO #495: UI file-write binding is not exposed".into(),
    ))
}

/// Persists prompt drafts/history through the bound private/config live-query files.
#[cfg(feature = "tui")]
pub async fn local_draft(_local: &LocalHarness, _input: &crate::tui::PromptInput) -> Result<()> {
    // TODO(#495): bind the prompt draft and history file writes, and load/follow
    // their initial snapshots into PromptInput before accepting terminal input.
    Err(Error::Unsupported(
        "TODO #495: prompt draft/history volume binding".into(),
    ))
}

/// Follow the local host's ordinary command/workspace files into the prompt.
#[cfg(feature = "tui")]
pub async fn local_completions(
    local: &LocalHarness,
    input: &mut crate::tui::PromptInput,
) -> Result<crate::tui_files::CompletionFiles> {
    // TODO(#495): take this resident query bound from the followed client settings.
    crate::tui_files::CompletionFiles::observe(local.task_files(), input, None, 4096).await
}

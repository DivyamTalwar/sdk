//! Local bindings shared by native clients; presentation never owns a host.
use super::{FileClientState, HarnessClient};
use crate::{Error, OperationId, Result, local::LocalHarness, wire};
use std::sync::Arc;

/// Opens the local client over the same task-volume request and follow binding
/// used by hosted clients, with private durable client state.
pub async fn connect_local(
    _local: Arc<LocalHarness>,
    _operation: OperationId,
) -> Result<(HarnessClient<FileClientState>, wire::ObserveRequest)> {
    // TODO(#645): expose the LocalHarness task-file admission/replay binding,
    // owner/control scope, initial cursor, private client-state path and file reader.
    Err(Error::Unsupported(
        "TODO #645: LocalHarness admission/replay binding is not exposed".into(),
    ))
}

/// Stages a prompt as a request file and returns its exact wire command identity.
pub async fn local_prompt(
    _local: &LocalHarness,
    _operation: OperationId,
    _prompt: &str,
) -> Result<wire::CommandEnvelope> {
    // TODO(#645): expose prompt request-file preparation without admitting or
    // executing it, so the existing client outbox remains the only submit path.
    Err(Error::Unsupported(
        "TODO #645: prompt admission binding is not exposed".into(),
    ))
}

/// Follow the local host's ordinary command/workspace files into the prompt.
#[cfg(feature = "tui")]
pub async fn local_completions(
    local: &LocalHarness,
    input: &mut crate::tui::PromptInput,
) -> Result<crate::tui_files::CompletionFiles> {
    // TODO(#495): take this resident query bound from the followed client settings.
    crate::tui_files::CompletionFiles::observe_authorized(local.task_files(), input, None, 4096)
        .await
}

/// Open the file-backed UI session. Its identity is supplied by the client binding.
#[cfg(feature = "tui")]
pub async fn local_ui(
    local: &LocalHarness,
    operation: &str,
    owner: &wire::Authority,
) -> Result<super::CommandSession<acyclic_stream::LocalStream>> {
    if owner.kind != wire::AggregateKind::Task as i32 {
        return Err(Error::Invalid("local UI requires a task owner".into()));
    }
    let session = uuid::Uuid::from_bytes(OperationId::parse(operation)?.into_bytes());
    // TODO(#495): obtain the stable UI-session identity and bounds from client settings.
    let mut files =
        super::CommandSession::open_authorized(local.task_files().clone(), session, 65_536, 4096)
            .await?;
    if files.selected_task()?.is_none() {
        let task = crate::TaskId::parse(&owner.id)?;
        let mut layout = files
            .state(super::CommandFileRoot::Session, "layout.json")?
            .unwrap_or_else(|| serde_json::json!({}));
        layout["selected_task"] = serde_json::Value::String(task.to_string());
        files
            .write_state(super::CommandFileRoot::Session, "layout.json", &layout)
            .await?;
    }
    Ok(files)
}

/// Follow the active local agent's goal/plans through its authorized file binding.
pub async fn local_documents(
    local: &LocalHarness,
    authority: &wire::Authority,
) -> Result<acyclic_stream::BoxProviderStream<'static, Result<super::ViewUpdate>>> {
    // TODO(#495): obtain document count/content/work bounds from followed settings.
    let files = local.scratch_files(acyclic_fs::performance::BOUNDARY_BUDGET)?;
    super::document_files(&files, authority, 4096, 65_536).await
}

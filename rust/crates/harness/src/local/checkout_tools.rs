//! Default commands bound to the selected, identity-fenced checkout.
use super::*;
use crate::{
    runtime::ToolContext,
    tool::{ToolExecutor, ToolInvocation, ToolResult},
};
use acyclic_stream::BoxProviderFuture;
use workspace::{AsyncMutex, OwnedMutexGuard, State, StateArc, lock_owned};

mod bash;
pub(super) mod capture;
mod git;
mod output;
#[cfg(all(test, acyclic_harness_loom))]
mod tests;

pub(super) struct CheckoutTools {
    workspace: Arc<LocalWorkspace>,
    read_grant: String,
    write_grant: String,
    clock: Arc<dyn acyclic_stream::UnixMillisClock>,
    task_files: Arc<crate::task_volume::TaskVolume<LocalStream>>,
    // Every binding shares the workspace's mount, branch and capture owner.
    state: StateArc<AsyncMutex<State>>,
}

// Keep the whole command, including its publication and cursor replacement,
// inside one guard. The async model exercises this same boundary.
async fn serialized<T, R>(
    state: &StateArc<AsyncMutex<T>>,
    command: impl AsyncFnOnce(OwnedMutexGuard<T>) -> R,
) -> R {
    command(lock_owned(StateArc::clone(state)).await).await
}

impl CheckoutTools {
    pub(super) fn new(
        workspace: Arc<LocalWorkspace>,
        project: &VolumeRef,
        clock: Arc<dyn acyclic_stream::UnixMillisClock>,
        task_files: Arc<crate::task_volume::TaskVolume<LocalStream>>,
    ) -> Result<Self> {
        if workspace
            .filesystem
            .workspace_id(&project.storage_name(&mut None)?)
            .map_err(acyclic_fs::WorkspaceError::from)
            .map_err(Error::from)?
            != workspace.repository
        {
            return Err(Error::Unauthorized(
                "workspace differs from the bound project volume".into(),
            ));
        }
        Ok(Self {
            read_grant: project.capability(VolumeOperation::Read, &mut None)?,
            write_grant: project.capability(VolumeOperation::Write, &mut None)?,
            state: StateArc::clone(&workspace.state),
            workspace,
            clock,
            task_files,
        })
    }
}

impl ToolExecutor for CheckoutTools {
    fn authorize(
        &self,
        _scope: Option<&RuntimeScope>,
        context: Option<&ToolContext>,
        invocation: &ToolInvocation,
        work: crate::NativeWork<'_>,
    ) -> Result<()> {
        if work.is_some() {
            return Err(Error::NativeBoundary {
                code: crate::wire::ErrorCode::Unsupported,
                reason: "local checkout commands require ordinary task execution",
            });
        }
        let context = context
            .ok_or_else(|| Error::Unauthorized("checkout command requires a task".into()))?;
        let scope = context.effective_scope();
        if !matches!(invocation.name.as_str(), "bash" | "fs")
            || context.operation_id() != invocation.operation_id
            || context.call_id() != invocation.call_id
            || !scope.grants().contains(&self.read_grant)
            || !scope.grants().contains(&self.write_grant)
            || !scope.grants().contains(if invocation.name == "bash" {
                "tool:call:bash"
            } else {
                "tool:call:fs"
            })
        {
            return Err(Error::Unauthorized(
                "checkout command differs from its task authority".into(),
            ));
        }
        Ok(())
    }

    fn execute<'a>(
        &'a self,
        mut invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            self.authorize(None, context.as_ref(), &invocation, None)?;
            let request = if invocation.name == "bash" {
                serde_json::to_vec(&invocation).map_err(storage)?
            } else {
                Vec::new()
            };
            let bash = if invocation.name == "bash" {
                let input: crate::tools::BashInput =
                    serde_json::from_value(std::mem::take(&mut invocation.arguments))
                        .map_err(|cause| Error::Invalid(cause.to_string()))?;
                input.validate()?;
                Some(input)
            } else {
                None
            };
            let command = if bash.is_none() {
                let command = invocation
                    .arguments
                    .as_object_mut()
                    .and_then(|arguments| arguments.remove("command"));
                let Some(serde_json::Value::String(command)) = command else {
                    return Err(Error::Invalid("checkout command must be text".into()));
                };
                if command.is_empty() || command.contains('\0') {
                    return Err(Error::Invalid(
                        "checkout command is empty or contains NUL".into(),
                    ));
                }
                Some(command)
            } else {
                None
            };
            let context = context
                .as_ref()
                .ok_or_else(|| Error::Unauthorized("checkout task is absent".into()))?;
            let maximum = context.effective_scope().limits().render_bytes;
            let maximum = usize::try_from(maximum).map_err(storage)?;
            let owner = context.task().ordinary_journal_owner::<LocalStream>()?;
            serialized(&self.state, async |mut state| {
                owner.verify(false, &mut None).await?;
                self.capture(&mut state, invocation.operation_id, 0, &owner)
                    .await?;
                if let Some(command) = command {
                    let result = self.fs(&mut state, &command, context).await?;
                    Ok(result)
                } else {
                    let input =
                        bash.ok_or_else(|| Error::Invalid("bash input is absent".into()))?;
                    let files =
                        output::Output::open(context, Arc::clone(&self.task_files), request)
                            .await?;
                    let (result, mut state, mut files) =
                        self.bash(input, maximum, state, files).await?;
                    self.capture(&mut state, invocation.operation_id, 1, &owner)
                        .await?;
                    if let Ok(result) = &result {
                        files.finish(result).await?;
                    }
                    result
                }
            })
            .await
        })
    }

    fn reconcile<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Option<ToolResult>>> {
        Box::pin(async move {
            self.authorize(None, context.as_ref(), &invocation, None)?;
            // A dispatched process must never be launched again after a lost ack.
            Ok(None)
        })
    }
}

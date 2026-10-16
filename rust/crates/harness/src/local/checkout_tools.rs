//! Default commands bound to the selected, identity-fenced checkout.
use super::*;
use crate::sync::owned::{Arc as StateArc, Mutex as AsyncMutex, OwnedMutexGuard, lock_owned};
use crate::{
    runtime::ToolContext,
    tool::{ToolExecutor, ToolInvocation, ToolResult},
};
use acyclic_fs::{NativeWatch, PublicationPermit};
use acyclic_stream::BoxProviderFuture;

mod bash;
mod capture;
mod git;
pub(super) mod publisher;
pub(super) mod reconciliation;
#[cfg(all(test, acyclic_harness_loom))]
mod tests;

pub(super) struct CheckoutTools {
    workspace: Arc<LocalWorkspace>,
    signed: Scope,
    read_grant: String,
    write_grant: String,
    capture_options: acyclic_fs::CaptureOptions,
    capture_policy: acyclic_fs::CapturePolicy,
    clock: Arc<dyn acyclic_stream::UnixMillisClock>,
    // Lock order: checkout state is outermost; command-local git merge cache
    // and switched-view locks are innermost and never held together. Native
    // completion locks are also inner; neither callback reacquires state.
    state: StateArc<AsyncMutex<State>>,
}

struct State {
    active: Option<workspace::View>,
    watcher: Option<capture::Cursor>,
}

// Keep the whole command, including its publication and cursor replacement,
// inside one guard. The async model exercises this same boundary.
async fn serialized<T, R>(
    state: &StateArc<AsyncMutex<T>>,
    command: impl AsyncFnOnce(OwnedMutexGuard<T>) -> R,
) -> R {
    command(lock_owned(StateArc::clone(state)).await).await
}

// A submitted blocking job owns the guard until its body and cleanup finish,
// even when its awaiting task is cancelled.
fn guarded<T, R>(
    mut state: OwnedMutexGuard<T>,
    operation: impl FnOnce(&mut T) -> R,
) -> (R, OwnedMutexGuard<T>) {
    let result = operation(&mut *state);
    (result, state)
}

impl CheckoutTools {
    pub(super) async fn new(
        workspace: Arc<LocalWorkspace>,
        project: &VolumeRef,
        clock: Arc<dyn acyclic_stream::UnixMillisClock>,
        signed: &Scope,
    ) -> Result<Self> {
        let (active, pending) = workspace.selected_view().await?;
        let (capture_options, capture_policy) = capture::configuration(&workspace)?;
        let view = active.as_ref().unwrap_or_else(|| workspace.view());
        reconciliation::recover_pending(&workspace, view, pending, &|| async { Ok(()) }).await?;
        let watcher = if workspace.fresh {
            Some(capture::Cursor::initial(&workspace, &capture_policy)?)
        } else {
            None
        };
        Ok(Self {
            signed: signed.clone(),
            read_grant: project.capability(VolumeOperation::Read, &mut None)?,
            write_grant: project.capability(VolumeOperation::Write, &mut None)?,
            capture_options,
            capture_policy,
            workspace,
            clock,
            state: StateArc::new(AsyncMutex::new(State { active, watcher })),
        })
    }
}

impl CheckoutTools {
    async fn require_access(
        &self,
        workspace: &acyclic_fs::Workspace<LocalAuthorityBackend, LocalObjectBackend>,
        ceiling: &Capabilities,
        surface: &'static str,
    ) -> Result<()> {
        use crate::filesystem::{PreparationIo, access};
        let io = PreparationIo::for_unmeasured(&acyclic_fs::CancellationToken::new())?;
        let label = crate::core::filesystem_scope_label(self.signed.capabilities(), &mut None)?;
        let payload = match label {
            Some(_) => access::capability_payload(label, VolumeOperation::Read, &io)?,
            None => include_bytes!("../filesystem/access/legacy-checkout.json"),
        };
        let compiled =
            access::access_view(workspace, &self.signed, None, payload, Some(ceiling), &io).await?;
        if compiled.0.generation_sensitive() || compiled.0.valid_until().is_some() {
            return Err(Error::FilesystemAccessConfiguration(surface));
        }
        access::require_unrestricted(&compiled.0, &acyclic_fs::access::Action::ALL, surface)
    }

    async fn require_repository_access(
        &self,
        selected: acyclic_fs::WorkspaceId,
        ceiling: &Capabilities,
    ) -> Result<()> {
        let state = acyclic_fs::GitCompatStore::load(
            &self.workspace.store,
            self.workspace.view().workspace().id(),
        )
        .await
        .map_err(storage)?;
        let Some(state) = state else {
            return Ok(());
        };
        // A raw facade cannot safely recover an opaque pending filesystem action.
        if state.pending.is_some() {
            return Err(Error::FilesystemAccessConfiguration("local fs facade"));
        }
        let mut workspaces = std::collections::BTreeSet::new();
        workspaces.extend(state.branches.values().map(|branch| branch.workspace_id));
        workspaces.extend(state.commits.values().flat_map(|commit| {
            [
                commit.tree.workspace_id(),
                commit.workspace_tree.workspace_id(),
            ]
        }));
        workspaces.extend(state.stash.iter().map(|tree| tree.workspace_id()));
        if let Some((_, pending)) = state.checkout_reconciliation {
            workspaces.insert(pending.workspace_id);
        }
        workspaces.remove(&selected);
        for id in workspaces {
            let workspace = acyclic_fs::WorkspaceResolver::resolve(
                &(
                    self.workspace.filesystem.clone(),
                    acyclic_fs::WorkspaceGraph::new(self.workspace.store.clone()),
                ),
                id,
            )
            .await
            .map_err(storage)?;
            self.require_access(&workspace, ceiling, "local fs facade")
                .await?;
        }
        Ok(())
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
            let context = context
                .as_ref()
                .ok_or_else(|| Error::Unauthorized("checkout task is absent".into()))?;
            let maximum = context.effective_scope().limits().render_bytes;
            let maximum = usize::try_from(maximum).map_err(storage)?;
            let result = serialized(&self.state, async |mut state| {
                let view = state
                    .active
                    .as_ref()
                    .unwrap_or_else(|| self.workspace.view());
                self.require_access(
                    view.workspace(),
                    context.effective_scope().grants(),
                    if invocation.name == "fs" {
                        "local fs facade"
                    } else {
                        "local bash"
                    },
                )
                .await?;
                if invocation.name == "fs" {
                    self.require_repository_access(
                        view.workspace().id(),
                        context.effective_scope().grants(),
                    )
                    .await?;
                }
                let owner = context.task().owned_mailbox.as_ref().ok_or_else(|| {
                    Error::Unauthorized("checkout command has no execution owner".into())
                })?;
                owner.verify_owner(None)?.await?;
                if reconciliation::recover(&self.workspace, view, &|| async {
                    owner.verify_owner(None)?.await
                })
                .await?
                {
                    state.watcher = None;
                }
                if invocation.name == "fs" {
                    self.fs(&command, context, &mut state).await
                } else {
                    let State { active, watcher } = &mut *state;
                    let view = active.as_ref().unwrap_or_else(|| self.workspace.view());
                    self.capture(
                        view,
                        watcher,
                        invocation.operation_id,
                        0,
                        PublicationPermit::Unrestricted,
                    )
                    .await?;
                    let (result, mut state) = self.bash(command, maximum, state).await?;
                    let State { active, watcher } = &mut *state;
                    let view = active.as_ref().unwrap_or_else(|| self.workspace.view());
                    self.capture(
                        view,
                        watcher,
                        invocation.operation_id,
                        1,
                        PublicationPermit::Unrestricted,
                    )
                    .await?;
                    owner.verify_owner(None)?.await?;
                    result
                }
            })
            .await;
            #[cfg(test)]
            if let Err(error) = &result {
                eprintln!("checkout command {}: {error:?}", invocation.name);
            }
            result
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

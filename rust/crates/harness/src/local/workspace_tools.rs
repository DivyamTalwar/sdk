use super::*;
use crate::{
    runtime::ToolContext,
    tool::{ToolExecutor, ToolInvocation, ToolResult},
};
use acyclic_fs::PublicationPermit;
use acyclic_stream::BoxProviderFuture;

#[cfg(test)]
mod properties;
mod shell;

pub(super) async fn open(
    project: &LocalProject,
    workspace: Option<Arc<LocalWorkspace>>,
    directory: &Path,
    volume: &VolumeRef,
    filesystem: acyclic_fs::LocalFs,
) -> Result<Arc<dyn ToolExecutor>> {
    let workspace = match workspace {
        Some(workspace) => workspace,
        None => Arc::new(workspace::open_volume(filesystem, volume, directory).await?),
    };
    Ok(Arc::new(WorkspaceTools {
        workspace,
        project_grants: project_grants(volume)?,
        materialized: matches!(project, LocalProject::Workspace(_)),
    }))
}

struct WorkspaceTools {
    workspace: Arc<LocalWorkspace>,
    project_grants: [String; 2],
    materialized: bool,
}

impl ToolExecutor for WorkspaceTools {
    fn authorize(
        &self,
        _: Option<&RuntimeScope>,
        context: Option<&ToolContext>,
        invocation: &ToolInvocation,
        work: crate::NativeWork<'_>,
    ) -> Result<()> {
        if work.is_some() {
            return Err(Error::NativeBoundary {
                code: crate::wire::ErrorCode::Unsupported,
                reason: "local workspace tools require ordinary execution",
            });
        }
        let context = context
            .ok_or_else(|| Error::Unauthorized("workspace task context is absent".into()))?;
        authorize_call(
            context.effective_scope(),
            &self.project_grants,
            invocation,
            context.operation_id(),
            context.call_id(),
        )
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<ToolResult>> {
        Box::pin(self.execute_call(invocation, context))
    }

    fn reconcile<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Option<ToolResult>>> {
        Box::pin(async move {
            self.authorize(None, context.as_ref(), &invocation, None)?;
            // The execution journal owns completed results. An interrupted process
            // cannot be dispatched again without knowing its outcome.
            Ok(None)
        })
    }
}

fn project_grants(volume: &VolumeRef) -> Result<[String; 2]> {
    Ok([
        volume.capability(VolumeOperation::Read, &mut None)?,
        volume.capability(VolumeOperation::Write, &mut None)?,
    ])
}

fn authorize_call(
    scope: &RuntimeScope,
    project_grants: &[String; 2],
    invocation: &ToolInvocation,
    operation: OperationId,
    call: &str,
) -> Result<()> {
    let grant = match invocation.name.as_str() {
        "bash" => "tool:call:bash",
        "fs" => "tool:call:fs",
        _ => return Err(Error::Unauthorized("unknown workspace tool".into())),
    };
    if invocation.operation_id != operation
        || invocation.call_id != call
        || !scope.grants().contains(grant)
    {
        return Err(Error::Unauthorized(
            "workspace invocation differs from its admitted call".into(),
        ));
    }
    for grant in project_grants {
        if !scope.grants().contains(grant) {
            return Err(Error::Unauthorized(
                "workspace command lacks selected project authority".into(),
            ));
        }
    }
    Ok(())
}

impl WorkspaceTools {
    async fn execute_call(
        &self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> Result<ToolResult> {
        self.authorize(None, context.as_ref(), &invocation, None)?;
        let context = context
            .ok_or_else(|| Error::Unauthorized("workspace task context is absent".into()))?;
        let command = invocation
            .arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::Invalid("workspace command is absent".into()))?;
        let owner = context.task().owned_mailbox.as_ref().ok_or_else(|| {
            Error::Unauthorized("workspace command has no execution owner".into())
        })?;
        owner.verify_owner(None)?.await?;
        let value = match invocation.name.as_str() {
            "bash" => Box::pin(shell::run(&self.workspace, command, &context)).await?,
            "fs" => Box::pin(self.git(command)).await?,
            _ => return Err(Error::Invalid("unknown workspace tool".into())),
        };
        owner.verify_owner(None)?.await?;
        Ok(ToolResult { value })
    }

    async fn git(&self, command: &str) -> Result<serde_json::Value> {
        use crate::tools::git::{
            RootMaterializingGitExecutor, WorkspaceGitExecutor, git_apply_patch, git_policy,
            git_workspace_tree, run_git_command,
        };
        let mut argv = crate::tools::parse_command(command)?;
        if argv.first().is_some_and(|arg| arg == "git") {
            argv.remove(0);
        }
        let workspace = &self.workspace;
        let lazy = workspace.view();
        let id = lazy.workspace().id();
        let tree = Box::pin(git_workspace_tree(lazy, &argv, None))
            .await
            .map_err(storage)?;
        let patch = Box::pin(git_apply_patch(lazy, &workspace.path, &argv))
            .await
            .map_err(storage)?;
        let (ignore, merge_drivers) = Box::pin(git_policy(lazy)).await.map_err(storage)?;
        let inner = WorkspaceGitExecutor {
            fs: &workspace.filesystem,
            store: &workspace.store,
            current: lazy.workspace().clone(),
            lazy_current: lazy,
            repository_id: id,
            ignore,
            permit: PublicationPermit::Unrestricted,
            lease: None,
            merge_drivers,
            merge_cache: tokio::sync::Mutex::new(Default::default()),
            switched: std::sync::Mutex::new(None),
        };
        let repository = acyclic_fs::GitCompatRepository::new(id, workspace.store.clone());
        let output = if self.materialized {
            let executor = RootMaterializingGitExecutor {
                inner,
                root: &workspace.path,
                source_root_identity: workspace.identity,
            };
            Box::pin(run_git_command(
                &repository,
                &argv,
                tree,
                patch.as_deref(),
                "local",
                0,
                &executor,
            ))
            .await
        } else {
            Box::pin(run_git_command(
                &repository,
                &argv,
                tree,
                patch.as_deref(),
                "local",
                0,
                &inner,
            ))
            .await
        }
        .map_err(storage)?
        .ok_or_else(|| Error::Conflict("recovered a pending git transition".into()))?;
        serde_json::to_value(output).map_err(storage)
    }
}

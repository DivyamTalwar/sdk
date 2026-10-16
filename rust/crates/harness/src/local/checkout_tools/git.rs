//! The shared git facade over the active checkout view.
use super::*;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

impl CheckoutTools {
    pub(super) async fn fs(
        &self,
        view: &workspace::View,
        command: &str,
        context: &ToolContext,
        watcher: &mut Option<capture::Cursor>,
    ) -> Result<(ToolResult, Option<workspace::View>)> {
        let owner = context
            .task()
            .owned_mailbox
            .as_ref()
            .ok_or_else(|| Error::Unauthorized("git command has no execution owner".into()))?;
        owner.verify_owner(None)?.await?;
        let (task, fence) = owner.task_binding();
        let lease_id = lease_identity(task, context.operation_id(), fence);
        let coordinator = acyclic_fs::OperationWindowCoordinator::new(
            self.workspace.filesystem.operation_window_store(),
        );
        let workspace = view.workspace();
        let parent = workspace
            .head(
                acyclic_fs::WorkBudget::UNBOUNDED,
                &acyclic_fs::CancellationToken::new(),
            )
            .await
            .map_err(storage)?
            .value;
        let now = acyclic_stream::UnixMillisClock::now_unix_millis(
            &acyclic_stream::SystemUnixMillisClock,
        );
        const GIT_PUBLICATION_LEASE_MILLIS: u64 = 60_000;
        let lease = coordinator
            .begin_with_lease_id(
                workspace.id(),
                parent.id(),
                context.operation_id().to_string(),
                now,
                now.saturating_add(GIT_PUBLICATION_LEASE_MILLIS),
                lease_id,
            )
            .await
            .map_err(storage)?;
        let result = async {
            owner.verify_owner(None)?.await?;
            self.capture(
                view,
                watcher,
                context.operation_id(),
                0,
                lease.publication_permit(),
            )
            .await?;
            self.fs_leased(view, command, context, &lease).await
        }
        .await;
        let closed = coordinator
            .finish_workspace(
                workspace,
                &lease,
                acyclic_stream::UnixMillisClock::now_unix_millis(
                    &acyclic_stream::SystemUnixMillisClock,
                ),
                acyclic_fs::OperationReconcileLimits::default(),
            )
            .await
            .map_err(storage);
        let result = result?;
        closed?;
        Ok(result)
    }

    async fn fs_leased(
        &self,
        view: &workspace::View,
        command: &str,
        context: &ToolContext,
        lease: &acyclic_fs::OperationWindowLease,
    ) -> Result<(ToolResult, Option<workspace::View>)> {
        use crate::tools::git::*;
        let mut argv = crate::tools::parse_command(command)?;
        if argv.first().is_some_and(|arg| arg == "git") {
            argv.remove(0);
        }
        let repository_id = self.workspace.view().workspace().id();
        let tree = git_workspace_tree(view, &argv, Some(lease.publication_permit())).await?;
        let patch = git_apply_patch(view, &self.workspace.path, &argv).await?;
        let (ignore, merge_drivers) = git_policy(view).await?;
        let executor = RootMaterializingGitExecutor {
            inner: WorkspaceGitExecutor {
                fs: &self.workspace.filesystem,
                store: &self.workspace.store,
                current: view.workspace().clone(),
                lazy_current: view,
                repository_id,
                ignore,
                permit: lease.publication_permit(),
                lease: Some(lease.clone()),
                merge_drivers,
                merge_cache: AsyncMutex::new(Default::default()),
                switched: Mutex::new(None),
            },
            root: &self.workspace.path,
            source_root_identity: self.workspace.identity,
            before_effect: || async {
                let owner = context.task().owned_mailbox.as_ref().ok_or_else(|| {
                    Error::Unauthorized("git command has no execution owner".into())
                })?;
                owner.verify_owner(None)?.await
            },
        };
        let repository =
            acyclic_fs::GitCompatRepository::new(repository_id, self.workspace.store.clone());
        let output = run_git_command(
            &repository,
            &argv,
            tree,
            patch.as_deref(),
            "LocalHarness",
            i64::try_from(self.clock.now_unix_millis() / 1000).unwrap_or(i64::MAX),
            &executor,
        )
        .await?
        .ok_or_else(|| {
            Error::Conflict("recovered a pending fs transition; retry the command".into())
        })?;
        let switched = executor.inner.switched_workspace()?;
        Ok((
            ToolResult {
                value: serde_json::json!({"result": output}),
            },
            switched,
        ))
    }
}

fn lease_identity(
    task: TaskId,
    operation: OperationId,
    fence: &crate::scheduler::LeaseFence,
) -> acyclic_fs::OperationLeaseId {
    let mut hash = blake3::Hasher::new_derive_key("acyclic.local.git-task-fence/v1");
    hash.update(&task.into_bytes());
    hash.update(&operation.into_bytes());
    for field in [&fence.placement, &fence.reservation_id] {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    let mut identity = [0; 16];
    identity.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    acyclic_fs::OperationLeaseId::from_bytes(identity)
}

#[cfg(all(test, not(acyclic_harness_loom)))]
#[path = "git_tests.rs"]
mod tests;

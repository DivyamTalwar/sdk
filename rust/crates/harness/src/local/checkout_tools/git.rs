//! The shared git facade over the active checkout view.
use super::*;

impl CheckoutTools {
    pub(super) async fn fs(
        &self,
        command: &str,
        context: &ToolContext,
        state: &mut State,
    ) -> Result<ToolResult> {
        use acyclic_fs::access::Action;
        let parsed = crate::tools::parse_fs_command(command)?;
        let (relative, mount) = state.mounts.named(parsed.volume.as_deref())?;
        let read_only = matches!(
            parsed.arguments[0].as_str(),
            "diff" | "log" | "show" | "status"
        );
        let now = self.clock.now_unix_millis() / 1000;
        for (action, operation) in std::iter::once((Action::Read, VolumeOperation::Read))
            .chain((!read_only).then_some((Action::Write, VolumeOperation::Write)))
        {
            mount.check(action, "", now)?;
            if !context
                .effective_scope()
                .grants()
                .contains(&mount.view.volume.capability(operation, &mut None)?)
            {
                return Err(Error::Unauthorized(
                    "fs target volume is not granted".into(),
                ));
            }
        }
        if !relative.is_empty() {
            if !read_only && !matches!(parsed.arguments[0].as_str(), "merge" | "discard") {
                return Err(Error::Unsupported(
                    "--volume supports diff, log, show, status, merge and discard".into(),
                ));
            }
            let source = mount
                .view
                .source
                .as_ref()
                .ok_or_else(|| Error::NotFound("volume view is not bound".into()))?;
            let native = source
                .native
                .as_ref()
                .ok_or_else(|| Error::NotFound("volume mount is not bound".into()))?;
            let owner = context.task().ordinary_journal_owner::<LocalStream>()?;
            owner.verify(false, &mut None).await?;
            let result = crate::tools::git::run_with_guard(
                crate::tools::git::Workspace {
                    filesystem: &self.workspace.filesystem,
                    store: &self.workspace.store,
                    view: source.workspace.view(),
                    root: native.path(),
                    repository: source.workspace.workspace().id(),
                    lease: None,
                    checkout: None,
                    author: "LocalHarness",
                    authored_at_seconds: i64::try_from(now).unwrap_or(i64::MAX),
                },
                &parsed.arguments,
                || async {
                    context
                        .task()
                        .ordinary_journal_owner::<LocalStream>()?
                        .verify(false, &mut None)
                        .await
                },
            )
            .await;
            // A failed merge may have published conflict markers.
            native.advance_to_head().await?;
            owner.verify(false, &mut None).await?;
            let output = result?;
            let command = output.command.ok_or_else(|| {
                Error::Conflict("recovered a pending fs transition; retry the command".into())
            })?;
            return Ok(ToolResult {
                value: serde_json::json!({"result":command}),
            });
        }
        let State {
            active, watcher, ..
        } = state;
        let view = active.as_ref().unwrap_or_else(|| self.workspace.view());
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
        let now = acyclic_stream::UnixMillisClock::now_unix_millis(
            &acyclic_stream::SystemUnixMillisClock,
        );
        let before_recovery = head(view).await?;
        self.prepare_reconciliation(context, b"recover", &before_recovery)
            .await?;
        // A previous worker can disappear after opening its lease. Expiry
        // fences it; complete its durable reconciliation before opening ours.
        let recovery = coordinator
            .recover_workspace(
                workspace,
                now,
                acyclic_fs::OperationReconcileLimits::default(),
            )
            .await
            .map_err(storage)?;
        let parent = match recovery {
            Some(outcome) => reconciliation_generation(outcome)?,
            None => before_recovery.clone(),
        };
        self.publish_reconciliation(context).await?;
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
            self.fs_leased(view, &parsed.arguments, context, &lease)
                .await
        }
        .await;
        let before_close = head(view).await?;
        if !result
            .as_ref()
            .is_ok_and(|output| output.switched.is_some())
        {
            self.prepare_reconciliation(context, b"close", &before_close)
                .await?;
        }
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
        // Closing can rebase a child after the command has already published.
        // A switched checkout belongs to the destination, so never apply the
        // source workspace's reconciliation over it.
        let publication = match closed {
            Ok(acyclic_fs::WorkspaceOperationFinish::Reconciled(outcome)) => {
                match reconciliation_generation(outcome) {
                    Ok(_)
                        if !result
                            .as_ref()
                            .is_ok_and(|output| output.switched.is_some()) =>
                    {
                        self.publish_reconciliation(context).await
                    }
                    Ok(_) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };
        let output = result?;
        if let Some(switched) = output.switched {
            *active = Some(switched.into_checkout()?);
        }
        // Persist the selected view in memory even when a recovered transition
        // has no command result or closing the old workspace failed.
        publication?;
        let command = output.command.ok_or_else(|| {
            Error::Conflict("recovered a pending fs transition; retry the command".into())
        })?;
        Ok(ToolResult {
            value: serde_json::json!({"result": command}),
        })
    }

    async fn fs_leased(
        &self,
        view: &workspace::View,
        argv: &[String],
        context: &ToolContext,
        lease: &acyclic_fs::OperationWindowLease,
    ) -> Result<crate::tools::git::Output> {
        crate::tools::git::run_with_guard(
            crate::tools::git::Workspace {
                filesystem: &self.workspace.filesystem,
                store: &self.workspace.store,
                view: view.into(),
                root: &self.workspace.path,
                repository: self.workspace.view().workspace().id(),
                lease: Some(lease.clone()),
                checkout: Some(self.workspace.identity),
                author: "LocalHarness",
                authored_at_seconds: i64::try_from(self.clock.now_unix_millis() / 1000)
                    .unwrap_or(i64::MAX),
            },
            argv,
            || async {
                let owner = context.task().owned_mailbox.as_ref().ok_or_else(|| {
                    Error::Unauthorized("git command has no execution owner".into())
                })?;
                owner.verify_owner(None)?.await
            },
        )
        .await
    }

    async fn prepare_reconciliation(
        &self,
        context: &ToolContext,
        phase: &[u8],
        from: &acyclic_fs::Generation<LocalAuthorityBackend, LocalObjectBackend>,
    ) -> Result<()> {
        let operation =
            reconciliation_operation(context.operation_id(), phase, from.id(), from.id());
        reconciliation::prepare(
            &self.workspace,
            operation,
            acyclic_fs::GitGenerationRef {
                workspace_id: from.workspace().id(),
                generation: from.id(),
            },
        )
        .await
    }

    async fn publish_reconciliation(&self, context: &ToolContext) -> Result<()> {
        let state = self.workspace.selected_view().await?.0;
        let view = state.as_ref().unwrap_or_else(|| self.workspace.view());
        reconciliation::publish(&self.workspace, view, &|| async {
            context
                .task()
                .owned_mailbox
                .as_ref()
                .ok_or_else(|| Error::Unauthorized("git command has no execution owner".into()))?
                .verify_owner(None)?
                .await
        })
        .await?;
        Ok(())
    }
}

pub(super) fn reconciliation_generation(
    outcome: acyclic_fs::WorkspaceRebase<LocalAuthorityBackend, LocalObjectBackend>,
) -> Result<acyclic_fs::Generation<LocalAuthorityBackend, LocalObjectBackend>> {
    match outcome {
        acyclic_fs::WorkspaceRebase::Rebased(generation)
        | acyclic_fs::WorkspaceRebase::AlreadyRebased(generation)
        | acyclic_fs::WorkspaceRebase::Current(generation) => Ok(generation),
        _ => Err(Error::Conflict(
            "checkout reconciliation did not settle".into(),
        )),
    }
}

fn reconciliation_operation(
    operation: OperationId,
    phase: &[u8],
    from: acyclic_fs::GenerationId,
    to: acyclic_fs::GenerationId,
) -> acyclic_fs::OperationId {
    let mut hash = blake3::Hasher::new_derive_key("acyclic.local.git-reconciliation/v1");
    hash.update(&operation.into_bytes());
    hash.update(phase);
    hash.update(from.digest().as_bytes());
    hash.update(to.digest().as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    acyclic_fs::OperationId::from_bytes(bytes)
}

async fn head(
    view: &workspace::View,
) -> Result<acyclic_fs::Generation<LocalAuthorityBackend, LocalObjectBackend>> {
    view.workspace()
        .head(
            acyclic_fs::WorkBudget::UNBOUNDED,
            &acyclic_fs::CancellationToken::new(),
        )
        .await
        .map(|receipt| receipt.value)
        .map_err(storage)
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

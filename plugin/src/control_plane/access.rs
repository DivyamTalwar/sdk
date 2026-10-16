//! Host-managed mounts use the service-authenticated route, never request claims.
use super::*;
use acyclic_harness::{
    Capabilities,
    core::{AggregateKind, Authority, AuthorityIssuer},
};

pub(super) async fn root_workspace(fs: &LocalFs, name: &str) -> Result<LocalWorkspace, String> {
    let mut operation = [0; 16];
    operation.copy_from_slice(&blake3::hash(name.as_bytes()).as_bytes()[..16]);
    fs.create_workspace_with_operation::<_, (), acyclic_fs::WorkspaceError>(
        name,
        acyclic_fs::VolumeInitialization {
            config: VolumeConfig::native(Lifecycle::Durable),
            access_policy: Some(acyclic_harness::filesystem::INITIAL_ACCESS_POLICY),
        },
        OperationId::from_bytes(operation),
        WorkBudget::UNBOUNDED,
        &CancellationToken::new(),
        None,
    )
    .await
    .map(|receipt| receipt.value.workspace().clone())
    .map_err(|failure| display(failure.error))
}

pub(super) async fn mount_options(
    workspace: &LocalWorkspace,
    agent: &str,
) -> Result<MountOptions, String> {
    use acyclic_fs::access::{Action, Capability, Generations, Grant, ResourcePattern};
    use acyclic_harness::filesystem::{NativeAccessAuthority, authenticated_mount_options};
    use ring::rand::SecureRandom as _;
    acyclic_fs::initialize_native_mount_services().map_err(display)?;
    acyclic_native_runtime::initialize_native_io_services().map_err(display)?;
    let cancellation = CancellationToken::new();
    let work = acyclic_fs::NativeMaterializationWork::new(WorkBudget::UNBOUNDED, &cancellation)
        .map_err(|(cause, _)| display(cause))?;
    // The private service catalog authorizes this exact route. Its capability
    // names only this workspace, and the persisted volume policy still limits it.
    let capability = Capability {
        layers: vec![vec![Grant {
            actions: Action::ALL.into_iter().collect(),
            resource: ResourcePattern {
                lineage_root: None,
                volume: Some(
                    uuid::Uuid::from_bytes(workspace.id().volume_id().into_bytes()).to_string(),
                ),
                generations: Generations::All,
                path: "**".into(),
            },
        }]],
        expires_at: None,
    };
    let grants = Capabilities::new([capability.to_scope_label().map_err(display)?]);
    let mut key = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut key)
        .map_err(display)?;
    let issuer = AuthorityIssuer::new(
        format!("host-managed:{agent}"),
        key,
        Authority {
            kind: AggregateKind::Agent,
            id: agent.into(),
        },
    );
    key.fill(0);
    let verifier = issuer.verifier(&mut None).map_err(display)?;
    let scope = issuer
        .issue("native-route", &grants, None, None, &mut None)
        .map_err(display)?;
    let generation = workspace
        .head(&work, &cancellation)
        .await
        .map_err(display)?
        .id();
    authenticated_mount_options(
        workspace,
        generation,
        NativeAccessAuthority {
            verifier: &verifier,
            scope: &scope,
            ceiling: &grants,
        },
        true,
        &work,
    )
    .await
    .map(|options| {
        options
            .publication(acyclic_fs::MountPublication::Manual)
            .native_context(&work, None, Some(&work))
    })
    .map_err(display)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acyclic_fs::access::{Action, Effect, Policy};

    #[tokio::test]
    async fn host_managed_routes_bind_current_policy_and_exact_workspace() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(display)?;
        let fs = LocalFs::local(acyclic_fs::LocalOptions::new(root.path()))
            .await
            .map_err(display)?;
        let workspace = root_workspace(&fs, "route").await?;
        let first = mount_options(&workspace, "authenticated-agent").await?;
        let view = first
            .require_access_view()
            .map_err(display)?
            .current()
            .map_err(display)?;
        let now = acyclic_stream::SystemUnixMillisClock;
        let now = acyclic_stream::UnixMillisClock::now_unix_millis(&now) / 1000;
        assert!(view.decide(Action::Read, "secrets/key", now).allowed);
        assert_eq!(
            view.volume(),
            uuid::Uuid::from_bytes(workspace.id().volume_id().into_bytes()).to_string()
        );
        let mut policy: Policy =
            serde_json::from_slice(acyclic_harness::filesystem::INITIAL_ACCESS_POLICY)
                .map_err(display)?;
        policy.version += 1;
        let mut deny = policy.layers[0][0].clone();
        deny.id = "hidden".into();
        deny.effect = Effect::Deny;
        deny.actions = [Action::Read, Action::List].into_iter().collect();
        deny.resource.path = "secrets/**".into();
        policy.layers[0].push(deny);
        let cancel = CancellationToken::new();
        let mut checkout = workspace
            .checkout(
                GenerationSelector::Head,
                CheckoutMode::tracking_transaction(),
                WorkBudget::UNBOUNDED,
                &cancel,
            )
            .await
            .map_err(display)?
            .value;
        checkout
            .write_access_policy(
                &serde_json::to_vec(&policy).map_err(display)?,
                acyclic_fs::NamedAttributeWriteMode::Replace,
                WorkBudget::UNBOUNDED,
                &cancel,
            )
            .await
            .map_err(display)?;
        checkout
            .commit(
                OperationId::new(),
                None,
                None,
                None,
                PublicationPermit::Unrestricted,
                WorkBudget::UNBOUNDED,
                &cancel,
                |_| {
                    std::future::ready(Ok::<_, acyclic_fs::OperationFailure<acyclic_fs::FsError>>(
                        acyclic_fs::OperationReceipt {
                            value: (),
                            work: Default::default(),
                        },
                    ))
                },
            )
            .await
            .map_err(display)?;
        // Both recovery and subagent creation use these same authenticated options.
        for owner in ["recovered-agent", "pending-child"] {
            let options = mount_options(&workspace, owner).await?;
            let view = options
                .require_access_view()
                .map_err(display)?
                .current()
                .map_err(display)?;
            assert!(!view.decide(Action::Read, "secrets/key", now).allowed);
            assert!(!view.decide(Action::List, "secrets/key", now).allowed);
            assert!(view.decide(Action::Read, "public/key", now).allowed);
            assert_eq!(view.policy_version(), policy.version);
        }
        Ok(())
    }
}

use super::*;
use proptest::prelude::*;

#[tokio::test]
async fn workspace_dispatch_futures_stay_bounded() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let model = Model::new(
        "scripted".into(),
        "sizes".into(),
        "1".into(),
        serde_json::Value::Null,
    )?;
    let provider = Arc::new(
        crate::test_support::ScriptedModelProvider::new([crate::test_support::ScriptedTurn::Text(
            "done".into(),
        )])
        .with_context_capacity(crate::context::ModelContextCapacity {
            context_tokens: 32_768,
            output_tokens: 4096,
        }),
    );
    let harness = Box::pin(LocalHarness::open(LocalHarnessConfig::new(
        directory.path(),
        (model, provider),
    )))
    .await?;
    let operation = OperationId::new();
    let Admission::Accepted(RuntimeTask::Durable { task_id, .. }) =
        harness.submit(operation, "sizes").await?
    else {
        return Err(Error::Conflict("size admission".into()));
    };
    let context = harness
        .runtime
        .harness()
        .durable_context(task_id, operation, None)
        .await?;
    let context = ToolContext::new(context, OperationId::new(), "call", None)?;
    let workspace = harness
        .workspace
        .clone()
        .ok_or_else(|| Error::NotFound("workspace".into()))?;
    let executor = WorkspaceTools {
        workspace,
        project_grants: project_grants(harness.project())?,
        materialized: true,
    };
    let invocation = ToolInvocation {
        operation_id: context.operation_id(),
        call_id: "call".into(),
        name: "bash".into(),
        arguments: serde_json::json!({"command":"printf sizes"}),
    };
    let dispatch = executor.execute_call(invocation, Some(context.clone()));
    let git = executor.git("status");
    let shell = shell::run(&executor.workspace, "printf sizes", &context);
    let sizes = [
        std::mem::size_of_val(&dispatch),
        std::mem::size_of_val(&git),
        std::mem::size_of_val(&shell),
    ];
    eprintln!("workspace dispatch/git/shell future sizes: {sizes:?}");
    assert!(
        sizes.iter().all(|size| *size < 16 * 1024),
        "workspace future sizes: {sizes:?}"
    );
    drop((dispatch, git, shell));
    let cancellation = acyclic_fs::CancellationToken::new();
    let work = acyclic_fs::NativeMaterializationWork::new(
        acyclic_fs::WorkBudget::UNBOUNDED,
        &cancellation,
    )
    .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
    let generation = Box::pin(executor.workspace.view().exactify(
        "/",
        PublicationPermit::Unrestricted,
        &mut acyclic_fs::kernel::KernelAccounting::NativeOriginal {
            work: &work,
            cancellation: &cancellation,
        },
    ))
    .await
    .map_err(storage)?;
    let output = tempfile::tempdir().map_err(storage)?;
    let options = acyclic_fs::MaterializeOptions::native(output.path());
    let mount = acyclic_fs::MountOptions::read_write()
        .publication(acyclic_fs::MountPublication::Manual)
        .native_context(&work, None, None);
    let view = Box::pin(generation.prepare_native_working_set(
        &mount,
        &options,
        acyclic_fs::WorkBudget::UNBOUNDED,
        &cancellation,
    ))
    .await
    .map_err(storage)?;
    let root = acyclic_fs::MountPath::root();
    let capture = view.capture_host_subtree(&root, &work);
    let sync = view.sync_with_permit(PublicationPermit::Unrestricted, &work, |_| {
        std::future::ready(Ok::<(), Error>(()))
    });
    let sizes = [
        std::mem::size_of_val(&capture),
        std::mem::size_of_val(&sync),
    ];
    eprintln!("native capture/sync future sizes: {sizes:?}");
    assert!(
        sizes.iter().all(|size| *size < 16 * 1024),
        "native capture/sync sizes: {sizes:?}"
    );
    drop((capture, sync));
    view.retire(&cancellation).map_err(storage)?;
    Ok(())
}

proptest! {
    // Property: the selected project and tool grants are all necessary; no
    // operation/call substitution or grant subset authorizes native IO.
    #[test]
    fn workspace_authority_is_the_intersection_of_identity_and_project_grants(
        grants in 0_u8..8,
        same_operation in any::<bool>(),
        same_call in any::<bool>(),
        bash in any::<bool>(),
    ) {
        let provider = crate::resources::ProviderRef::new("test".into(), "filesystem".into(), "2".into(), &mut None)?;
        let volume = VolumeRef::new(provider, "project".into(), VolumeClass::Project,
            VolumeOwner::Project("test".into()), &mut None)?;
        let name = if bash { "bash" } else { "fs" };
        let required = [format!("tool:call:{name}"),
            volume.capability(VolumeOperation::Read, &mut None)?,
            volume.capability(VolumeOperation::Write, &mut None)?];
        let capabilities = Capabilities::new(required.iter().enumerate()
            .filter(|(index, _)| grants & (1 << index) != 0).map(|(_, grant)| grant.as_str()));
        let scope = RuntimeScope::new(capabilities, Limits::default())?;
        let operation = OperationId::from_bytes([1; 16]);
        let invocation = ToolInvocation {
            operation_id: if same_operation { operation } else { OperationId::from_bytes([2;16]) },
            call_id: if same_call { "call" } else { "other" }.into(),
            name: name.into(), arguments: serde_json::json!({"command":"printf test"}),
        };
        prop_assert_eq!(authorize_call(&scope, &project_grants(&volume)?, &invocation, operation, "call").is_ok(),
            grants == 7 && same_operation && same_call);
    }
}

use super::*;
use acyclic_harness::runtime::RuntimeScope;

#[tokio::test]
async fn runtime_scope_cannot_drop_a_ceiling_and_keep_legacy_authority()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new().await?;
    let scoped = fixture.narrowed(false, false)?;
    let legacy = Capabilities::new([fixture
        .volume
        .capability(VolumeOperation::Read, &mut None)?]);
    assert!(!legacy.is_subset_of(scoped.capabilities()));
    assert!(
        fixture
            .issuer
            .attenuate(&scoped, "escape", &legacy)
            .is_err()
    );
    let runtime = RuntimeScope::new(scoped.capabilities().clone(), Default::default())?;
    assert!(runtime.narrow(legacy.clone(), runtime.limits()).is_err());
    let fs = Capabilities::new(
        scoped
            .capabilities()
            .iter()
            .filter(|label| label.starts_with(SCOPE_PREFIX)),
    );
    for reduced in [
        scoped.capabilities().intersect(&legacy),
        scoped.capabilities().without(&fs),
    ] {
        assert!(reduced.iter().all(|label| !is_filesystem_capability(label)));
    }
    // Completely relinquishing filesystem access remains a valid attenuation.
    assert!(Capabilities::default().is_subset_of(scoped.capabilities()));
    Ok(())
}

#[tokio::test]
async fn task_ceiling_reaches_the_broader_bound_reader() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new().await?;
    let reader = fixture.reader(&fixture.scope)?;
    for (path, expired, expected) in [
        (true, false, true),
        (false, false, false),
        (true, true, false),
    ] {
        let caller = fixture.narrowed(path, expired)?;
        let task =
            context::content_context(reader.clone(), None, caller.capabilities().clone()).await?;
        assert_eq!(task.read_file(&fixture.file, None).await.is_ok(), expected);
    }
    // The additional caller cannot replace the signed reader's narrower ceiling.
    let bound = fixture.narrowed(false, false)?;
    let task = context::content_context(
        fixture.reader(&bound)?,
        None,
        fixture.scope.capabilities().clone(),
    )
    .await?;
    assert!(task.read_file(&fixture.file, None).await.is_err());
    Ok(())
}

#[tokio::test]
async fn structured_only_runtime_authority_uses_the_filesystem_owner() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new().await?;
    let all = fixture.narrowed(true, false)?;
    let grants = Capabilities::new(
        all.capabilities()
            .iter()
            .filter(|label| label.starts_with(SCOPE_PREFIX)),
    );
    let task = context::content_context(fixture.reader(&fixture.scope)?, None, grants).await?;
    assert_eq!(
        task.read_file(&fixture.file, None).await?.as_ref(),
        b"private"
    );
    fixture.policy(true, 2).await?;
    assert!(task.read_file(&fixture.file, None).await.is_err());
    Ok(())
}

#[tokio::test]
async fn empty_signed_or_caller_ceiling_stays_empty() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new().await?;
    let empty = fixture.issuer.issue(
        "empty",
        &Capabilities::new([
            fixture
                .volume
                .capability(VolumeOperation::Read, &mut None)?,
            Capability {
                layers: vec![],
                expires_at: None,
            }
            .to_scope_label()?,
        ]),
        fixture.scope.agent(),
        None,
        &mut None,
    )?;
    let all = fixture.narrowed(true, false)?;
    for (signed, caller) in [(&empty, &all), (&all, &empty)] {
        let task =
            context::content_context(fixture.reader(signed)?, None, caller.capabilities().clone())
                .await?;
        assert!(task.read_file(&fixture.file, None).await.is_err());
    }
    Ok(())
}

#[tokio::test]
#[cfg(not(target_arch = "wasm32"))]
async fn caller_ceiling_and_its_storage_use_the_original_native_ledger()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new().await?;
    let reader = fixture.reader(&fixture.scope)?;
    let cancellation = CancellationToken::new();
    let work = acyclic_fs::NativeMaterializationWork::new(
        acyclic_fs::performance::BOUNDARY_BUDGET,
        &cancellation,
    )
    .map_err(|(cause, work)| acyclic_harness::Error::NativeConstruction { cause, work })?;
    let _ = work.receipt();
    let baseline = work.live_allocation_bytes();
    for allowed in [true, false] {
        let caller = fixture.narrowed(allowed, false)?;
        let io = acyclic_harness::filesystem::PreparationIo::from_native_work(&work)?;
        let result = reader
            .read(&fixture.file, 4096, Some(caller.capabilities()), Some(&io))?
            .await;
        assert_eq!(result.is_ok(), allowed);
        drop(result);
        drop(io);
        let _ = work.receipt();
        assert_eq!(work.live_allocation_bytes(), baseline);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn runtime_read_is_bound_policy_and_both_capability_ceilings(
        signed_path in any::<bool>(), caller_path in any::<bool>(),
        signed_expired in any::<bool>(), caller_expired in any::<bool>(), denied in any::<bool>(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        runtime.block_on(async {
            let fixture = Fixture::new().await?;
            fixture.policy(denied, 2).await?;
            let signed = fixture.narrowed(signed_path, signed_expired)?;
            let caller = fixture.narrowed(caller_path, caller_expired)?;
            let task = context::content_context(fixture.reader(&signed)?, None, caller.capabilities().clone()).await?;
            assert_eq!(task.read_file(&fixture.file, None).await.is_ok(),
                signed_path && caller_path && !signed_expired && !caller_expired && !denied);
            Ok::<_, Box<dyn Error>>(())
        }).map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

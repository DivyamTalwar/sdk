use super::*;

async fn fixture() -> Result<(tempfile::TempDir, LocalHarness, CheckoutPublisher)> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let model = Model::new(
        "scripted".into(),
        "publication".into(),
        "1".into(),
        serde_json::Value::Null,
    )?;
    let provider = Arc::new(crate::test_support::ScriptedModelProvider::new([]));
    let harness =
        LocalHarness::open(LocalHarnessConfig::new(directory.path(), (model, provider))).await?;
    let tools = Arc::new(
        CheckoutTools::new(
            harness
                .workspace
                .as_ref()
                .ok_or_else(|| Error::NotFound("checkout".into()))?
                .clone(),
            &harness.project,
            Arc::new(acyclic_stream::SystemUnixMillisClock),
        )
        .await?,
    );
    let publisher = CheckoutPublisher::new(
        tools,
        harness.files.clone(),
        &harness.project,
        &harness.verifier,
        &harness.signed,
        harness.limits.file_bytes,
    );
    Ok((directory, harness, publisher))
}

#[tokio::test]
async fn file_publication_materializes_its_exact_committed_bytes() -> Result<()> {
    let (directory, harness, publisher) = fixture().await?;
    let (file, _) = publisher
        .stage_once(
            OperationId::new(),
            "published.txt",
            b"exact bytes",
            "text/plain",
            "published.txt",
            None,
            None,
        )?
        .await?;
    assert_eq!(file.volume(), &harness.project);
    assert_eq!(
        std::fs::read(directory.path().join("published.txt")).map_err(storage)?,
        b"exact bytes"
    );
    assert!(!directory.path().join(".system").exists());
    Ok(())
}

#[tokio::test]
async fn open_reads_checkout_selection_once_when_no_publication_is_pending() -> Result<()> {
    let (_directory, harness, publisher) = fixture().await?;
    let workspace = publisher.tools.workspace.clone();
    let before = workspace
        .selection_reads
        .load(std::sync::atomic::Ordering::Relaxed);
    CheckoutTools::new(
        workspace.clone(),
        &harness.project,
        Arc::new(acyclic_stream::SystemUnixMillisClock),
    )
    .await?;
    assert_eq!(
        workspace
            .selection_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        before + 1,
        "open reloaded Git state after selecting the checkout"
    );
    Ok(())
}

#[tokio::test]
async fn committed_uploads_recover_checkout_before_capture_and_never_reapply_completed_edits()
-> Result<()> {
    let (directory, harness, publisher) = fixture().await?;
    let view = harness.workspace.as_ref().unwrap().view();
    let writer = publisher.writer(view)?;
    let operation = OperationId::new();
    // Crash boundary: the actual volume write commits, but no native
    // preparation journal or checkout edit has happened.
    let (original, _) = writer
        .stage_once(
            operation,
            "once.txt",
            b"once",
            "text/plain",
            "once.txt",
            None,
            None,
        )?
        .await?;
    assert!(!directory.path().join("once.txt").exists());
    // Recovery normally enters through observe_staged_once, before executing
    // the tool again. It must repair the checkout as well as return its ref.
    assert_eq!(
        publisher
            .observe_staged_once(operation, 1024, None, None)?
            .await?
            .0
            .as_ref(),
        Some(&original)
    );
    assert_eq!(
        std::fs::read(directory.path().join("once.txt")).map_err(storage)?,
        b"once"
    );
    assert_eq!(
        publisher
            .stage_once(
                operation,
                "once.txt",
                b"once",
                "text/plain",
                "once.txt",
                None,
                None,
            )?
            .await?
            .0,
        original
    );

    let operation = OperationId::new();
    let (original, _) = writer
        .stage(
            operation,
            "path.txt",
            b"path",
            "text/plain",
            "path.txt",
            None,
            None,
        )?
        .await?;
    assert!(!directory.path().join("path.txt").exists());
    assert_eq!(
        publisher
            .stage(
                operation,
                "path.txt",
                b"path",
                "text/plain",
                "path.txt",
                None,
                None,
            )?
            .await?
            .0,
        original
    );
    assert_eq!(
        std::fs::read(directory.path().join("path.txt")).map_err(storage)?,
        b"path"
    );

    let operation = OperationId::new();
    let (replacement, _) = writer
        .stage_at(operation, &original, b"replacement", None, None)?
        .await?;
    assert_eq!(
        std::fs::read(directory.path().join("path.txt")).map_err(storage)?,
        b"path"
    );
    assert_eq!(
        publisher
            .stage_at(operation, &original, b"replacement", None, None)?
            .await?
            .0,
        replacement
    );
    assert_eq!(
        std::fs::read(directory.path().join("path.txt")).map_err(storage)?,
        b"replacement"
    );
    let cancellation = acyclic_fs::CancellationToken::new();
    let before = view
        .workspace()
        .head(acyclic_fs::WorkBudget::UNBOUNDED, &cancellation)
        .await
        .map_err(storage)?
        .value
        .id();
    std::fs::write(directory.path().join("path.txt"), b"later user edit").map_err(storage)?;
    assert_eq!(
        publisher
            .stage_at(operation, &original, b"replacement", None, None)?
            .await?
            .0,
        replacement
    );
    assert_eq!(
        std::fs::read(directory.path().join("path.txt")).map_err(storage)?,
        b"later user edit"
    );
    // Git recovery may enter with the already-committed target as both
    // heads. It must use the saved source and retain later checkout edits too.
    let head = view
        .workspace()
        .head(acyclic_fs::WorkBudget::UNBOUNDED, &cancellation)
        .await
        .map_err(storage)?
        .value;
    crate::tools::git::publish_checkout_transition(
        directory.path(),
        publisher.tools.workspace.identity,
        &publisher.tools.workspace.store,
        publication_operation(operation, original.path()),
        head.clone(),
        head,
        &|| std::future::ready(Ok(())),
    )
    .await?;
    assert_eq!(
        std::fs::read(directory.path().join("path.txt")).map_err(storage)?,
        b"later user edit"
    );
    let after = view
        .workspace()
        .head(acyclic_fs::WorkBudget::UNBOUNDED, &cancellation)
        .await
        .map_err(storage)?
        .value
        .id();
    assert_eq!(
        before, after,
        "retry captured the missing native write or authored another upload"
    );
    Ok(())
}

#[tokio::test]
async fn one_operation_publishes_each_path_and_retries_without_overwriting_edits() -> Result<()> {
    let (directory, _harness, publisher) = fixture().await?;
    let operation = OperationId::new();
    let mut files = Vec::new();
    for (path, bytes) in [
        ("first.txt", b"first".as_slice()),
        ("second.txt", b"second".as_slice()),
    ] {
        let (file, _) = publisher
            .stage(operation, path, bytes, "text/plain", path, None, None)?
            .await?;
        assert_eq!(
            std::fs::read(directory.path().join(path)).map_err(storage)?,
            bytes
        );
        files.push((file, bytes));
    }
    for (file, bytes) in files {
        std::fs::write(directory.path().join(file.path()), "later edit").map_err(storage)?;
        let (retried, _) = publisher
            .stage(
                operation,
                file.path(),
                bytes,
                "text/plain",
                file.display_name(),
                None,
                None,
            )?
            .await?;
        assert_eq!(retried, file);
        assert_eq!(
            std::fs::read(directory.path().join(file.path())).map_err(storage)?,
            b"later edit"
        );
    }
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(8))]
    #[test]
    fn checkout_publication_preserves_caller_ceilings(path in "file-[a-z]{1,8}\\.txt", allowed in proptest::bool::ANY) {
        tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(|error| proptest::test_runner::TestCaseError::fail(error.to_string()))?.block_on(async {
            let (_directory, harness, publisher) = fixture().await?;
            let ceiling = if allowed { harness.signed.capabilities().clone() } else { crate::Capabilities::default() };
            let operation = OperationId::new();
            let (source, _) = publisher.stage_once(operation, &path, b"original", "text/plain", &path, None, None)?.await?;
            // Capture a real unrelated edit before obtaining the immutable
            // replacement reference. stage_at must reject the earlier head.
            std::fs::write(_directory.path().join("user-sibling.txt"), b"user edit").map_err(storage)?;
            let fresh = serialized(&publisher.tools.state, async |mut state| {
                let State { active, watcher } = &mut *state;
                let view = active.as_ref().unwrap_or_else(|| publisher.tools.workspace.view());
                publisher.tools.capture(view, watcher, OperationId::new(), 0, PublicationPermit::Unrestricted).await?;
                let host = harness.files.for_workspace(&harness.project, view.workspace().clone())?;
                let grant = crate::conversation::ContentGrant::verify_directory_read(&harness.verifier, &harness.signed,
                    &harness.project, "", &mut None)?;
                let (fresh, bytes) = host.read_private_path(&harness.project, &grant, source.path(), None, 1024, None, None).await?;
                assert_eq!(bytes.as_ref(), b"original");
                Ok::<_, Error>(fresh)
            }).await?;
            assert_ne!(source.version(), fresh.version());
            assert!(matches!(publisher.stage_at(OperationId::new(), &source, b"stale", None, None)?.await,
                Err(Error::Conflict(_))));
            let replacement = publisher.stage_at(OperationId::new(), &fresh, b"replaced", Some(&ceiling), None)?.await;
            assert_eq!(replacement.is_ok(), allowed, "{:?}", replacement.as_ref().err());
            let observed = publisher.observe_staged_once(operation, 1024, Some(&ceiling), None)?.await;
            assert_eq!(observed.is_ok(), allowed, "{:?}", observed.as_ref().err());
            for once in [false, true] {
                let target = format!("{once}-{path}");
                let operation = OperationId::new();
                let result = if once {
                    publisher.stage_once(operation, &target, b"new", "text/plain", &target, Some(&ceiling), None)?.await
                } else {
                    publisher.stage(operation, &target, b"new", "text/plain", &target, Some(&ceiling), None)?.await
                };
                assert_eq!(result.is_ok(), allowed, "{:?}", result.as_ref().err());
                if allowed {
                    // A committed reply still needs the current caller's ceiling.
                    let empty = crate::Capabilities::default();
                    let replay = if once {
                        publisher.stage_once(operation, &target, b"new", "text/plain", &target, Some(&empty), None)?.await
                    } else {
                        publisher.stage(operation, &target, b"new", "text/plain", &target, Some(&empty), None)?.await
                    };
                    assert!(replay.is_err(), "cached publication bypassed the caller ceiling");
                }
            }
            Ok::<_, crate::Error>(())
        }).map_err(|error| proptest::test_runner::TestCaseError::fail(error.to_string()))?;
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn publication_identity_binds_operation_and_path(
        operation in proptest::prelude::any::<[u8;16]>(),
        path in "[a-z]{1,16}(/[a-z]{1,16}){0,3}",
    ) {
        let original = OperationId::from_bytes(operation);
        let first = publication_operation(original, &path);
        proptest::prop_assert_eq!(first, publication_operation(original, &path));
        proptest::prop_assert_ne!(first, publication_operation(original, &format!("{path}x")));
        let mut other = operation; other[0] ^= 1;
        proptest::prop_assert_ne!(first, publication_operation(OperationId::from_bytes(other), &path));
    }
}

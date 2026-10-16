use super::*;
use proptest::prelude::*;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn fixture() -> TestResult<(
    tempfile::TempDir,
    LocalFs,
    LocalCoreStateStore,
    LocalWorkspace,
)> {
    acyclic_native_runtime::initialize_native_io_services()?;
    let directory = tempfile::tempdir()?;
    let filesystem =
        LocalFs::local(acyclic_fs::LocalOptions::new(directory.path().join("fs"))).await?;
    let store = LocalCoreStateStore::new(directory.path().join("core"));
    let workspace = filesystem
        .create_workspace("project", WorkBudget::UNBOUNDED, &CancellationToken::new())
        .await?
        .value;
    WorkspaceGraph::new(store.clone())
        .register_root(&workspace)
        .await?;
    Ok((directory, filesystem, store, workspace))
}

async fn execute(
    filesystem: &LocalFs,
    store: &LocalCoreStateStore,
    view: View<'_>,
    repository: acyclic_fs::WorkspaceId,
    args: &[&str],
) -> crate::Result<Output> {
    run(
        Workspace {
            filesystem,
            store,
            view,
            root: Path::new("."),
            repository,
            lease: None,
            checkout: None,
            author: "test",
            authored_at_seconds: 1,
        },
        &args
            .iter()
            .map(|argument| (*argument).into())
            .collect::<Vec<_>>(),
    )
    .await
}

#[tokio::test]
async fn volume_branches_use_the_same_facade_without_a_native_source() -> TestResult {
    let (_directory, filesystem, store, main) = fixture().await?;
    main.write_text("/note", "base\n").await?;
    let repository = main.id();
    execute(
        &filesystem,
        &store,
        (&main).into(),
        repository,
        &["add", "note"],
    )
    .await?;
    execute(
        &filesystem,
        &store,
        (&main).into(),
        repository,
        &["commit", "-m", "base"],
    )
    .await?;
    let child = execute(
        &filesystem,
        &store,
        (&main).into(),
        repository,
        &["checkout", "-b", "topic"],
    )
    .await?
    .switched
    .ok_or("no selected branch")?;
    assert!(matches!(&child, OwnedView::Volume(_)));
    assert_eq!(
        GitCompatRepository::new(repository, store.clone())
            .current_workspace_id()
            .await?,
        child.workspace().id()
    );
    child.workspace().write_text("/note", "child\n").await?;
    execute(
        &filesystem,
        &store,
        child.view(),
        repository,
        &["add", "note"],
    )
    .await?;
    execute(
        &filesystem,
        &store,
        child.view(),
        repository,
        &["commit", "-m", "child"],
    )
    .await?;
    assert_eq!(
        View::Volume(&main).read("/note", 32).await?.as_ref(),
        b"base\n"
    );
    let selected = execute(
        &filesystem,
        &store,
        child.view(),
        repository,
        &["checkout", "main"],
    )
    .await?
    .switched
    .ok_or("no selected main branch")?;
    assert_eq!(selected.workspace().id(), main.id());
    assert_eq!(
        GitCompatRepository::new(repository, store.clone())
            .current_workspace_id()
            .await?,
        main.id()
    );
    execute(
        &filesystem,
        &store,
        selected.view(),
        repository,
        &["merge", "topic"],
    )
    .await?;
    assert_eq!(
        selected.view().read("/note", 32).await?.as_ref(),
        b"child\n"
    );
    Ok(())
}

#[test]
fn volume_view_pins_exact_generations_for_both_capture_modes() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let (_directory, _filesystem, _store, workspace) = runtime.block_on(fixture())?;
    // Property: for arbitrary UTF-8 bytes, exact and lazy requests over an SDK
    // volume select the identical current generation and return identical bytes.
    proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(32)).run(
        &".{0,64}",
        |text| {
            runtime.block_on(async {
                workspace.write_text("/note", &text).await.unwrap();
                let view = View::Volume(&workspace);
                let lazy = view
                    .tree(false, PublicationPermit::Unrestricted)
                    .await
                    .unwrap();
                let exact = view
                    .tree(true, PublicationPermit::Unrestricted)
                    .await
                    .unwrap();
                prop_assert_eq!(lazy, exact);
                prop_assert!(matches!(exact, GitTreeRef::Exact(_)));
                let bytes = view.read("/note", 256).await.unwrap();
                prop_assert_eq!(bytes.as_ref(), text.as_bytes());
                Ok(())
            })
        },
    )?;
    Ok(())
}

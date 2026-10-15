use acyclic_fs::{
    CancellationToken, GitCommandOutput, IdempotencyKey, LazyWorkspace, LocalCoreStateStore,
    LocalFs, LocalOptions, OperationId, PublicationPermit, WorkBudget,
    demand::native::NativeDemandSource,
    model::{Lifecycle, VolumeConfig},
};
use acyclic_harness::tools::git::{
    WorkspaceGitExecutor, git_policy, git_workspace_tree, run_git_command,
};
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;

type Lazy = LazyWorkspace<
    acyclic_fs::LocalAuthorityBackend,
    acyclic_fs::LocalObjectBackend,
    NativeDemandSource,
    LocalCoreStateStore,
>;

async fn run(
    fs: &LocalFs,
    store: &LocalCoreStateStore,
    lazy: &Lazy,
    repository_id: acyclic_fs::WorkspaceId,
    args: &[&str],
) -> (GitCommandOutput, Option<Lazy>) {
    let argv: Vec<_> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let tree = git_workspace_tree(lazy, &argv, None).await.unwrap();
    let (ignore, merge_drivers) = git_policy(lazy).await.unwrap();
    let executor = WorkspaceGitExecutor {
        fs,
        store,
        current: lazy.workspace().clone(),
        lazy_current: lazy,
        repository_id,
        ignore,
        permit: PublicationPermit::Unrestricted,
        lease: None,
        merge_drivers,
        merge_cache: AsyncMutex::new(Default::default()),
        switched: Mutex::new(None),
    };
    let output = run_git_command(
        &acyclic_fs::GitCompatRepository::new(repository_id, store.clone()),
        &argv,
        tree,
        None,
        "test",
        1,
        &executor,
    )
    .await
    .unwrap()
    .unwrap();
    (output, executor.switched_workspace().unwrap())
}

#[tokio::test]
async fn facade_commits_branches_merges_and_inspects_managed_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let fs = LocalFs::local(LocalOptions::new(temp.path().join("fs")))
        .await
        .unwrap();
    let store = LocalCoreStateStore::open_owned(temp.path().join("core")).unwrap();
    let config = VolumeConfig::native(Lifecycle::Durable);
    let stop = CancellationToken::new();
    let source = NativeDemandSource::open(
        Cow::Borrowed(&root),
        config.profile,
        config.limits,
        None,
        None,
        WorkBudget::UNBOUNDED,
        &stop,
    )
    .await
    .unwrap()
    .value;
    let mut work = WorkBudget::UNBOUNDED.accounting(&stop);
    let (main, _) = LazyWorkspace::attach(
        &fs,
        "main",
        Arc::new(source),
        store.clone(),
        config,
        OperationId::new(),
        &mut work,
    )
    .await
    .unwrap();
    main.write(
        "/file.txt",
        bytes::Bytes::from_static(b"base\n").into(),
        IdempotencyKey::new(),
        PublicationPermit::Unrestricted,
        &mut work,
    )
    .await
    .unwrap();
    let repo = main.workspace().id();
    assert!(matches!(
        run(&fs, &store, &main, repo, &["commit", "-m", "base"])
            .await
            .0,
        GitCommandOutput::Committed(_)
    ));
    let (_, child) = run(
        &fs,
        &store,
        &main,
        repo,
        &["checkout", "-b", "agents/child"],
    )
    .await;
    let child = child.unwrap();
    child
        .write(
            "/file.txt",
            bytes::Bytes::from_static(b"child\n").into(),
            IdempotencyKey::new(),
            PublicationPermit::Unrestricted,
            &mut work,
        )
        .await
        .unwrap();
    run(&fs, &store, &child, repo, &["commit", "-m", "child edit"]).await;
    run(&fs, &store, &child, repo, &["checkout", "main"]).await;
    run(&fs, &store, &main, repo, &["merge", "agents/child"]).await;
    assert_eq!(
        main.read("/file.txt", 64, &mut work)
            .await
            .unwrap()
            .bytes
            .as_ref(),
        b"child\n"
    );
    for args in [
        &["diff"][..],
        &["grep", "child"],
        &["archive"],
        &["blame", "file.txt"],
    ] {
        assert!(matches!(
            run(&fs, &store, &main, repo, args).await.0,
            GitCommandOutput::Filesystem(_)
        ));
    }
}

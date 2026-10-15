use super::*;
use acyclic_fs::{
    LazyWorkspace, LazyWorkspaceStore, LocalCoreStateStore, NativeMaterializationWork,
    demand::native::NativeDemandSource, kernel::KernelAccounting,
};

/// Lazy view over a held checkout root, with persistent source identity.
pub struct LocalWorkspace {
    pub(super) path: PathBuf,
    pub(super) directory: PathBuf,
    pub(super) filesystem: acyclic_fs::LocalFs,
    pub(super) store: LocalCoreStateStore,
    pub(super) identity: acyclic_fs::NativeRootIdentity,
    view: LazyWorkspace<
        LocalAuthorityBackend,
        LocalObjectBackend,
        NativeDemandSource,
        LocalCoreStateStore,
    >,
}

impl LocalWorkspace {
    /// Source-backed filesystem view. No checkout scan occurs during open.
    #[must_use]
    pub const fn view(
        &self,
    ) -> &LazyWorkspace<
        LocalAuthorityBackend,
        LocalObjectBackend,
        NativeDemandSource,
        LocalCoreStateStore,
    > {
        &self.view
    }
}

pub(super) async fn open(
    filesystem: &acyclic_fs::Fs<LocalAuthorityBackend, LocalObjectBackend>,
    volume: &VolumeRef,
    path: &Path,
    directory: &Path,
    identity: Option<[u8; 16]>,
    operation: acyclic_fs::OperationId,
) -> Result<LocalWorkspace> {
    let path = std::fs::canonicalize(path).map_err(storage)?;
    let cancellation = acyclic_fs::CancellationToken::new();
    let work = NativeMaterializationWork::new(acyclic_fs::WorkBudget::UNBOUNDED, &cancellation)
        .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
    let mut accounting = KernelAccounting::NativeOriginal {
        work: &work,
        cancellation: &cancellation,
    };
    let name = volume.storage_name(&mut None)?;
    let id = filesystem.workspace_id(&name).map_err(storage)?;
    let store = LocalCoreStateStore::new(directory.join("workspace"));
    let original = store
        .load_lazy_workspace(id, &mut accounting)
        .await
        .map_err(storage)?;
    let source = NativeDemandSource::open(
        std::borrow::Cow::Borrowed(&path),
        acyclic_fs::VolumeConfig::native(acyclic_fs::Lifecycle::Durable).profile,
        acyclic_fs::VolumeLimits::default(),
        original.as_ref().map(|state| state.source),
        None,
        &work,
        &cancellation,
    )
    .await
    .map_err(storage)?;
    if identity != Some(source.root_identity().to_bytes()) {
        return Err(Error::Unauthorized(
            "local checkout root was replaced during open".into(),
        ));
    }
    let source = Arc::new(source);
    let view = match original {
        Some(_) => {
            let workspace = filesystem
                .open_workspace(&name, &work, &cancellation)
                .await
                .map_err(storage)?;
            LazyWorkspace::open(workspace, source, store.clone(), &mut accounting)
                .await
                .map_err(storage)?
        }
        None => {
            LazyWorkspace::attach(
                filesystem,
                &name,
                source,
                store.clone(),
                acyclic_fs::VolumeConfig::native(acyclic_fs::Lifecycle::Durable),
                operation,
                &mut accounting,
            )
            .await
            .map_err(storage)?
            .0
        }
    };
    Ok(LocalWorkspace {
        path,
        directory: directory.to_path_buf(),
        view,
        filesystem: filesystem.clone(),
        store,
        identity: acyclic_fs::NativeRootIdentity::from_bytes(
            identity.ok_or_else(|| Error::Invalid("workspace root identity is absent".into()))?,
        ),
    })
}

pub(super) async fn open_volume(
    filesystem: acyclic_fs::LocalFs,
    volume: &VolumeRef,
    directory: &Path,
) -> Result<LocalWorkspace> {
    let path = directory.join("project-view");
    std::fs::create_dir_all(&path).map_err(storage)?;
    let cancellation = acyclic_fs::CancellationToken::new();
    let work = NativeMaterializationWork::new(acyclic_fs::WorkBudget::UNBOUNDED, &cancellation)
        .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
    let mut accounting = KernelAccounting::NativeOriginal {
        work: &work,
        cancellation: &cancellation,
    };
    let name = volume.storage_name(&mut None)?;
    let id = filesystem.workspace_id(&name).map_err(storage)?;
    let store = LocalCoreStateStore::new(directory.join("workspace"));
    let original = store
        .load_lazy_workspace(id, &mut accounting)
        .await
        .map_err(storage)?;
    let source = NativeDemandSource::open(
        std::borrow::Cow::Borrowed(&path),
        acyclic_fs::VolumeConfig::native(acyclic_fs::Lifecycle::Durable).profile,
        acyclic_fs::VolumeLimits::default(),
        original.as_ref().map(|state| state.source),
        None,
        &work,
        &cancellation,
    )
    .await
    .map_err(storage)?;
    let identity = source.root_identity();
    let workspace = filesystem
        .open_workspace(&name, &work, &cancellation)
        .await
        .map_err(storage)?;
    let view = LazyWorkspace::bind(workspace, Arc::new(source), store.clone(), &mut accounting)
        .await
        .map_err(storage)?;
    Ok(LocalWorkspace {
        path,
        directory: directory.to_path_buf(),
        view,
        filesystem,
        store,
        identity,
    })
}

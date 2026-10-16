use super::*;
use acyclic_fs::{
    LazyWorkspace, LazyWorkspaceStore, LocalCoreStateStore, NativeMaterializationWork,
    demand::native::NativeDemandSource, kernel::KernelAccounting,
};

mod mounts;
pub(super) mod volumes;

pub(super) struct MountedVolume {
    pub(super) workspace: crate::tools::git::OwnedView,
    pub(super) native: Option<acyclic_fs::Mount<LocalAuthorityBackend, LocalObjectBackend>>,
}
pub(super) type Mounts = crate::workspace::MountTable<Option<Arc<MountedVolume>>>;

pub(super) type View = LazyWorkspace<
    LocalAuthorityBackend,
    LocalObjectBackend,
    NativeDemandSource,
    LocalCoreStateStore,
>;

/// Lazy view over a held checkout root, with persistent source identity.
pub struct LocalWorkspace {
    pub(super) path: PathBuf,
    pub(super) filesystem: acyclic_fs::LocalFs,
    pub(super) store: LocalCoreStateStore,
    pub(super) identity: acyclic_fs::NativeRootIdentity,
    // Lock order: view state -> coordinator, released before task-file index
    // -> provider visibility -> provider state. Git cache/switch locks are
    // innermost and never held together. Callbacks never reacquire view state;
    // the Loom model exercises command and offloaded-cleanup custody.
    pub(super) state: super::checkout_tools::StateArc<
        super::checkout_tools::AsyncMutex<super::checkout_tools::State>,
    >,
    context_line: String,
    pub(super) repository: acyclic_fs::WorkspaceId,
}

pub(super) fn attach_operation(task: TaskId) -> acyclic_fs::OperationId {
    let mut hash = blake3::Hasher::new();
    hash.update(b"acyclic.local.checkout-attach/v1\0");
    hash.update(&task.into_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    acyclic_fs::OperationId::from_bytes(bytes)
}

impl LocalWorkspace {
    pub(super) fn context_line(&self) -> &str {
        &self.context_line
    }
    pub(super) fn context_stage(&self) -> Arc<dyn crate::context::ContextStage> {
        Arc::new(crate::context::SourceStage::new(
            "workspace_volumes",
            "1",
            Arc::new(crate::context::Context {
                messages: vec![crate::model::ModelMessage {
                    role: crate::model::ModelRole::System,
                    content: ModelContent::Text(self.context_line().to_owned()),
                }],
                ..Default::default()
            }),
            crate::context::ContextPlacement::Prepend,
        ))
    }

    /// Binds the native checkout executor to this owner-selected project volume.
    pub fn executor(
        self: &Arc<Self>,
        project: &VolumeRef,
        clock: Arc<dyn acyclic_stream::UnixMillisClock>,
        task_files: Arc<crate::task_volume::TaskVolume<LocalStream>>,
    ) -> Result<Arc<dyn crate::tool::ToolExecutor>> {
        Ok(Arc::new(super::checkout_tools::CheckoutTools::new(
            Arc::clone(self),
            project,
            clock,
            task_files,
        )?))
    }

    #[cfg(test)]
    pub(super) async fn primary(&self) -> Result<Arc<MountedVolume>> {
        let state =
            super::checkout_tools::lock_owned(super::checkout_tools::StateArc::clone(&self.state))
                .await;
        state
            .mounts
            .named(None)?
            .1
            .view
            .source
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::NotFound("primary view is not bound".into()))
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
    let source = source
        .exclude(
            volumes::excluded(&path, view_config())?,
            &&work,
            &cancellation,
        )
        .map_err(|error| Error::from(acyclic_fs::LazyWorkspaceError::from(error)))?;
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
    acyclic_fs::WorkspaceGraph::new(store.clone())
        .register_root(view.workspace())
        .await
        .map_err(storage)?;
    let repository = view.workspace().id();
    let selected = acyclic_fs::GitCompatRepository::new(repository, store.clone())
        .current_workspace_id()
        .await?;
    let view = if selected == repository {
        view
    } else {
        let selected = acyclic_fs::WorkspaceResolver::resolve(
            &(
                filesystem.clone(),
                acyclic_fs::WorkspaceGraph::new(store.clone()),
            ),
            selected,
        )
        .await?;
        view.open_related(selected, &mut accounting).await?
    };
    let mounts = mounts::owner(
        &path,
        volume,
        Some(Arc::new(MountedVolume {
            workspace: crate::tools::git::OwnedView::Checkout(view),
            native: None,
        })),
        [],
    )?;
    Ok(LocalWorkspace {
        repository,
        context_line: mounts.context_line(),
        state: super::checkout_tools::StateArc::new(super::checkout_tools::AsyncMutex::new(
            super::checkout_tools::State {
                mounts,
                capture: None,
            },
        )),
        path,
        filesystem: filesystem.clone(),
        store,
        identity: acyclic_fs::NativeRootIdentity::from_bytes(
            identity.ok_or_else(|| Error::Invalid("checkout identity is absent".into()))?,
        ),
    })
}

fn view_config() -> acyclic_fs::VolumeConfig {
    acyclic_fs::VolumeConfig::native(acyclic_fs::Lifecycle::Durable)
}

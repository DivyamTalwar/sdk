use super::*;
use acyclic_fs::{
    GenerationId, Workspace,
    access::{AccessView, PathTrie},
    native_mount::{NativeMountCallbackFuture, NativeMountCallbackGuard, NativeMountError},
};

struct PolicyOwner<A, O> {
    workspace: Workspace<A, O>,
    scope: Scope,
    context: Arc<crate::runtime::TaskContext>,
    generation: GenerationId,
    writable: bool,
    view: Arc<AccessView>,
    _custody: NativeAllocation,
}

async fn compile<A: AsyncAuthorityStore, O: AsyncObjectStore>(
    workspace: &Workspace<A, O>,
    scope: &Scope,
    context: &crate::runtime::TaskContext,
    generation: GenerationId,
    writable: bool,
    io: &PreparationIo,
) -> Result<(PathTrie, NativeAllocation)> {
    let label =
        crate::core::filesystem_scope_label(scope.capabilities(), &mut Some(&mut io.history_io()))?;
    let payload = match label {
        Some(_) => crate::filesystem::access::capability_payload(label, VolumeOperation::Read, io)?,
        None if writable => include_bytes!("../access/legacy-native.json"),
        None => crate::filesystem::access::capability_payload(None, VolumeOperation::Read, io)?,
    };
    let (view, holds, _) = crate::filesystem::access::access_view(
        workspace,
        scope,
        if writable { None } else { Some(generation) },
        payload,
        Some(context.scope().grants()),
        io,
    )
    .await?;
    let mut custody = io
        .original_native_work()
        .ok_or_else(|| io.invalid("native access lost its original ledger"))?
        .claim(0, 0, acyclic_fs::WorkCounters::UNCHARGED)?;
    for hold in holds.into_iter().flatten() {
        let (_, mut held) = crate::filesystem::native_decoded(((), hold))?;
        custody.absorb(&mut held)?;
    }
    Ok((view, custody))
}

impl<A, O> BorrowedNativeVolumeBinding<'_, A, O>
where
    A: AsyncAuthorityStore + Send + Sync + 'static,
    O: AsyncObjectStore + Send + Sync + 'static,
{
    /// Bind a native checkout to its verified scope and original caller ceiling.
    pub async fn mount_options(
        &self,
        workspace: &Workspace<A, O>,
        generation: GenerationId,
        context: &Arc<crate::runtime::TaskContext>,
        work: &NativeMaterializationWork,
    ) -> Result<acyclic_fs::MountOptions> {
        let io = PreparationIo::from_native_work(work)?;
        for operation in [VolumeOperation::Read]
            .into_iter()
            .chain(self.writable.then_some(VolumeOperation::Write))
        {
            ContentGrant::authorize_volume(
                self.verifier,
                self.scope,
                self.volume,
                operation,
                context.scope().limits().file_bytes,
                &mut Some(&mut io.history_io()),
            )?;
            let admitted = match operation {
                VolumeOperation::Read => crate::core::directory_read_granted_with_work(
                    context.scope().grants(),
                    self.volume,
                    "",
                    &mut Some(&mut io.history_io()),
                )?,
                VolumeOperation::Write => crate::core::volume_write_granted_with_work(
                    context.scope().grants(),
                    self.volume,
                    &mut Some(&mut io.history_io()),
                )?,
            };
            if !admitted {
                return Err(io.invalid("native caller lacks volume authority"));
            }
        }
        if !self.host.filesystem.owns_workspace(workspace)
            || self.volume.storage_name(&mut Some(&mut io.history_io()))?
                != workspace.name().as_str()
        {
            return Err(io.invalid("native access differs from its authenticated volume"));
        }
        self.generation.validate(&mut Some(&mut io.history_io()))?;
        if self.generation.as_resource().provider() != self.volume.provider()
            || self.generation.as_resource().key() != generation.digest().as_bytes()
        {
            return Err(io.invalid("native access differs from its selected generation"));
        }
        let (compiled, custody) = compile(
            workspace,
            self.scope,
            context,
            generation,
            self.writable,
            &io,
        )
        .await?;
        let view = AccessView::new_native(compiled, custody)?;
        let scope = self.scope.copy(&mut Some(&mut io.history_io()))?;
        io.retain_owned(
            (std::mem::size_of::<PolicyOwner<A, O>>() + 2 * std::mem::size_of::<usize>()) as u64,
        )?;
        io.allocation_attempt(
            (std::mem::size_of::<PolicyOwner<A, O>>() + 2 * std::mem::size_of::<usize>()) as u64,
        )?;
        let retained = io.owned_bytes()?;
        let refresh = Arc::new(PolicyOwner {
            workspace: workspace.clone(),
            scope,
            context: Arc::clone(context),
            generation,
            writable: self.writable,
            view: Arc::clone(&view),
            _custody: io.into_native_custody(retained)?,
        });
        Ok((if self.writable {
            acyclic_fs::MountOptions::read_write()
        } else {
            acyclic_fs::MountOptions::read_only()
        })
        .access_view(view)
        .access_refresh(refresh))
    }
}

impl<A, O> NativeMountCallbackGuard for PolicyOwner<A, O>
where
    A: AsyncAuthorityStore + Send + Sync + 'static,
    O: AsyncObjectStore + Send + Sync + 'static,
{
    fn check<'a>(
        &'a self,
        work: &'a NativeMaterializationWork,
    ) -> std::result::Result<NativeMountCallbackFuture<'a>, NativeMountError> {
        NativeMountCallbackFuture::new_guarded::<Error, _, _>(work, move || async move {
            if !self._custody.work().shares_ledger(work) {
                return Err(Error::NativeBoundary {
                    code: crate::wire::ErrorCode::Unauthorized,
                    reason: "native policy refresh changed its ledger",
                });
            }
            let io = PreparationIo::from_native_work(work)?;
            let (view, custody) = compile(
                &self.workspace,
                &self.scope,
                &self.context,
                self.generation,
                self.writable,
                &io,
            )
            .await?;
            self.view.publish_native(view, custody)?;
            Ok(())
        })
    }
    fn check_write<'a>(
        &'a self,
        work: &'a NativeMaterializationWork,
    ) -> std::result::Result<NativeMountCallbackFuture<'a>, NativeMountError> {
        self.check(work)
    }
}

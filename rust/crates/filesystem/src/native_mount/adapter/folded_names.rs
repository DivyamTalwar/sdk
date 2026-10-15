use super::*;

impl<A: AsyncAuthorityStore, O: AsyncObjectStore> CheckoutMountSource<A, O> {
    pub(super) async fn require_unique_folded_path(
        &self,
        checkout: &mut Checkout<A, O>,
        path: &NamespacePath,
        include_directory: bool,
        work: &crate::NativeMaterializationWork,
        cancellation: &CancellationToken,
    ) -> Result<(), MountSourceError> {
        let mut accounting = crate::kernel::KernelAccounting::native(work, cancellation);
        let mut parent = crate::kernel::Owned::new(
            NamespacePath::new(Vec::new(), self.limits).map_err(engine_error)?,
            accounting.claim_owned(0, 0)?,
            0,
        );
        for index in 0..=path.components().len() {
            if index == path.components().len() && !include_directory {
                break;
            }
            let record = checkout
                .lookup_no_follow(&parent.value, work, cancellation)
                .await?
                .record
                .ok_or(MountSourceError::NotFound)?;
            let FilePayload::Directory { entries } = record.payload else {
                return Err(MountSourceError::NotDirectory);
            };
            let unique = match self.folded_directories.get(entries) {
                Some(unique) => unique,
                None => {
                    let unique = checkout
                        .unique_folded_tree(entries, &mut accounting, cancellation)
                        .await?;
                    self.folded_directories.insert(entries, unique);
                    unique
                }
            };
            if !unique {
                return Err(MountSourceError::Filesystem(FsError::NameCollision));
            }
            if let Some(name) = path.components().get(index) {
                let (value, allocation) = parent
                    .value
                    .child_with_accounting(name, self.limits, &mut accounting, cancellation)
                    .map_err(engine_error)?;
                let bytes = value.retained_bytes();
                let next = crate::kernel::Owned::new(value, allocation, bytes);
                std::mem::replace(&mut parent, next).retire(&mut accounting)?;
            }
        }
        parent.retire(&mut accounting)?;
        Ok(())
    }
}

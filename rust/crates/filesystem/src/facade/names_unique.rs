use super::*;

impl<A: AsyncAuthorityStore, O: AsyncObjectStore> Checkout<A, O> {
    // One paged pass. Unique directories retain no name keys; legacy collisions
    // retain the sorted index so only ambiguous names are refused.
    pub(crate) async fn folded_collision_keys(
        &self,
        tree: ObjectId,
        accounting: &mut KernelAccounting<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Owned<Vec<Vec<u8>>>, FsError> {
        let mut keys = Owned::<Vec<Vec<u8>>>::allocate(0, accounting)?;
        // Entry count can exceed file count through hard links. Actual growth
        // is charged to the caller; this bound is only Vec's addressable size.
        let maximum = isize::MAX as usize / size_of::<Vec<u8>>();
        let mut after: Option<(LogicalName, KernelOwnedAllocation)> = None;
        loop {
            let (page, allocation) = crate::kernel::list_tree_entries_async_with_accounting(
                &self.volume.fs.inner.objects,
                tree,
                (
                    after.as_ref().map_or(std::ops::Bound::Unbounded, |name| {
                        std::ops::Bound::Excluded(&name.0)
                    }),
                    super::names::CASE_FOLD_COLLISION_SCAN_PAGE,
                ),
                DecodeLimits::for_volume(self.volume.config),
                accounting,
                cancellation,
                Some(&self.dependencies.data_custody),
            )
            .await?;
            for entry in &page.entries {
                accounting.charge(WorkCounters {
                    items_examined: 1,
                    ..WorkCounters::UNCHARGED
                })?;
                keys.ensure_for_push(maximum, accounting)?;
                let (key, mut owner) = super::names::name_fold_key(&entry.name, accounting)?;
                keys.bytes = keys
                    .bytes
                    .checked_add(key.capacity() as u64)
                    .ok_or(WorkError::Overflow)?;
                keys.allocation.absorb(&mut owner)?;
                keys.value.push(key);
            }
            if let Some(old) = after.take() {
                let bytes = old.0.retained_bytes() as u64;
                super::names::retire_name_owned(old, bytes, accounting)?;
            }
            let has_more = page.has_more;
            if has_more && let Some(entry) = page.entries.last() {
                after = Some(entry.name.clone_with_accounting(accounting, cancellation)?);
            }
            let bytes = (page.entries.capacity() * size_of::<crate::kernel::TreeEntry>()) as u64
                + page
                    .entries
                    .iter()
                    .map(|entry| entry.name.retained_bytes() as u64)
                    .sum::<u64>();
            super::names::retire_name_owned((page, allocation), bytes, accounting)?;
            if !has_more {
                break;
            }
        }
        keys.value.sort_unstable();
        if !keys.value.windows(2).any(|pair| pair[0] == pair[1]) {
            keys.retire(accounting)?;
            return Owned::allocate(0, accounting);
        }
        Ok(keys)
    }
}

// The sorted index retains duplicates. Binary search can return any equal key;
// its neighbors tell whether this native identity is ambiguous.
pub(crate) fn ambiguous_folded_name(
    keys: &[Vec<u8>],
    name: &LogicalName,
    accounting: &mut KernelAccounting<'_>,
) -> Result<bool, FsError> {
    if keys.is_empty() {
        return Ok(false);
    }
    let key = super::names::name_fold_key(name, accounting)?;
    let ambiguous = keys.binary_search(&key.0).is_ok_and(|index| {
        index
            .checked_sub(1)
            .is_some_and(|before| keys[before] == key.0)
            || keys.get(index + 1).is_some_and(|after| *after == key.0)
    });
    let bytes = key.0.capacity() as u64;
    super::names::retire_name_owned(key, bytes, accounting)?;
    Ok(ambiguous)
}

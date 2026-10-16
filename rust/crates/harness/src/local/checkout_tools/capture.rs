//! Push watcher hints captured in one filesystem generation commit.
use super::*;
use acyclic_fs::{
    CancellationToken, CaptureOptions, CapturePolicy, LivePublicationObservation,
    NativeWatchOptions, PublicationPermit, WatchBatch, WorkBudget, WorkCounters, capture_baseline,
    capture_watch_batch,
    kernel::NamespacePath,
    model::{CheckoutMode, GenerationSelector},
};
use std::time::Duration;

const EXCLUDED: &[&str] = &[".git", ".acyclic-sdk"];
const MAXIMUM_PATHS: u32 = 262_144;
const WATCH_FENCE_TIMEOUT: Duration = Duration::from_secs(1);

pub(in crate::local) struct CheckoutCapture {
    options: CaptureOptions,
    policy: CapturePolicy,
    watcher: Option<acyclic_fs::NativeWatch>,
}

impl CheckoutTools {
    pub(super) async fn capture(
        &self,
        state: &mut State,
        operation: OperationId,
        phase: u8,
        owner: &crate::durable_host::TaskJournalOwner<LocalStream>,
    ) -> Result<()> {
        for (_, mount) in state.mounts.mounts() {
            let source = mount
                .view
                .source
                .as_ref()
                .ok_or_else(|| Error::NotFound("volume view is not bound".into()))?;
            if let Some(native) = &source.native {
                owner.verify(false, &mut None).await?;
                native.sync().await?;
                native.advance_to_head().await?;
            }
        }
        let binding = state
            .mounts
            .named(None)?
            .1
            .view
            .source
            .as_ref()
            .ok_or_else(|| Error::NotFound("primary view is not bound".into()))?;
        let crate::tools::git::View::Checkout(view) = binding.workspace.view() else {
            return Ok(());
        };
        if state.capture.is_none() {
            let (options, policy) = configuration(
                &self.workspace,
                &state.mounts,
                view.workspace().volume_config(),
            )?;
            state.capture = Some(CheckoutCapture {
                options,
                policy,
                watcher: None,
            });
        }
        state
            .capture
            .as_mut()
            .ok_or_else(|| Error::NotFound("checkout capture is not bound".into()))?
            .capture(view, operation, phase, owner)
            .await
    }
}

impl CheckoutCapture {
    async fn capture(
        &mut self,
        view: &workspace::View,
        operation: OperationId,
        phase: u8,
        owner: &crate::durable_host::TaskJournalOwner<LocalStream>,
    ) -> Result<()> {
        let cancellation = CancellationToken::new();
        let workspace = view.workspace();
        let options = &self.options;
        let policy = &self.policy;
        // On failure discard the cursor: the next command must establish a new
        // baseline rather than skip changes consumed before a failed commit.
        let baseline = self.watcher.is_none();
        let mut watcher = if let Some(watcher) = self.watcher.take() {
            watcher
        } else {
            self.open_watcher(view)?
        };
        if watcher.root_identity() != options.expected_root_identity {
            return Err(Error::Unauthorized("checkout root was replaced".into()));
        }
        let mut checkout = workspace
            .checkout(
                GenerationSelector::Head,
                CheckoutMode::tracking_transaction(),
                WorkBudget::UNBOUNDED,
                &cancellation,
            )
            .await
            .map_err(storage)?
            .value;
        let batch = if baseline {
            None
        } else {
            watcher.fence(WATCH_FENCE_TIMEOUT).map_err(storage)?;
            Some(
                watcher
                    .poll(MAXIMUM_PATHS, WorkBudget::UNBOUNDED, &cancellation)
                    .map_err(storage)?
                    .value,
            )
        };
        let batch = match batch {
            Some(batch @ WatchBatch::Changes { .. }) => batch,
            None | Some(WatchBatch::RescanRequired { .. }) => {
                // A reset is an explicit requirement to replace the baseline.
                // Register push watches first, then include changes delivered
                // during the scan before committing the captured generation.
                watcher.begin_rescan().map_err(storage)?;
                watcher.watch_tree(EXCLUDED).map_err(storage)?;
                capture_baseline(
                    &mut checkout,
                    options,
                    policy,
                    WorkBudget::UNBOUNDED,
                    &cancellation,
                )
                .await
                .map_err(storage)?;
                watcher.finish_rescan().map_err(storage)?
            }
        };
        capture_watch_batch(
            &mut checkout,
            batch,
            options,
            policy,
            WorkBudget::UNBOUNDED,
            &cancellation,
        )
        .await
        .map_err(storage)?;
        if checkout.has_pending_mutations() {
            let operation = capture_operation(operation, phase);
            let committed = checkout
                .commit(
                    operation,
                    None,
                    None,
                    None,
                    PublicationPermit::Unrestricted,
                    WorkBudget::UNBOUNDED,
                    &cancellation,
                    |_| async {
                        owner
                            .verify(false, &mut None)
                            .await
                            .map_err(acyclic_fs::OperationFailure::before_work)?;
                        Ok::<_, acyclic_fs::OperationFailure<Error>>(acyclic_fs::OperationReceipt {
                            value: (),
                            work: WorkCounters::UNCHARGED,
                        })
                    },
                )
                .await
                .map_err(|failure| failure.error)?
                .value;
            if !matches!(
                committed,
                LivePublicationObservation::Committed { .. }
                    | LivePublicationObservation::AlreadyCommitted { .. }
            ) {
                return Err(Error::Conflict(
                    "checkout capture publication raced with another writer".into(),
                ));
            }
        }
        self.watcher = Some(watcher);
        Ok(())
    }

    fn open_watcher(&self, view: &workspace::View) -> Result<acyclic_fs::NativeWatch> {
        let config = view.workspace().volume_config();
        let mut watcher = acyclic_fs::NativeWatch::open_with_profile(
            &self.options.source_root,
            config.profile,
            NativeWatchOptions::new(config.limits),
        )
        .map_err(storage)?;
        watcher
            .exclude_subtrees(self.policy.excluded_prefixes())
            .map_err(storage)?;
        Ok(watcher)
    }
}

fn capture_operation(operation: OperationId, phase: u8) -> acyclic_fs::OperationId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"acyclic.local.checkout-capture/v1\0");
    hasher.update(&operation.into_bytes());
    hasher.update(&[phase]);
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    acyclic_fs::OperationId::from_bytes(bytes)
}

fn configuration(
    workspace: &LocalWorkspace,
    mounts: &workspace::Mounts,
    config: acyclic_fs::VolumeConfig,
) -> Result<(CaptureOptions, CapturePolicy)> {
    let options = CaptureOptions {
        source_root: workspace.path.clone(),
        expected_root_identity: workspace.identity,
        maximum_paths: MAXIMUM_PATHS,
        maximum_extent_spans: 65_536,
    };
    let mut excluded = EXCLUDED
        .iter()
        .copied()
        .chain(
            mounts
                .mounts()
                .map(|(path, _)| path)
                .filter(|path| !path.is_empty()),
        )
        .map(|name| {
            let path = acyclic_fs::path::PortablePath::parse(&format!("/{name}"), config.limits)
                .map_err(storage)?;
            NamespacePath::from_portable_in_profile(&path, config.profile, config.limits)
                .map_err(storage)
        })
        .collect::<Result<Vec<_>>>()?;
    excluded.extend(workspace::volumes::excluded(&workspace.path, config)?);
    let policy = CapturePolicy::excluding(excluded).map_err(storage)?;
    Ok((options, policy))
}

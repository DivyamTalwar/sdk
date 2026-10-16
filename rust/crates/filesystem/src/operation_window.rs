//! Durable operation windows for stable mounted-tool execution.
//!
//! A window is a short-lived coordination scope, not a filesystem lock. Any
//! number of tools may read and write the same mounted workspace while leases
//! are active. Parent advances are coalesced and reconciled exactly once after
//! the last lease closes. Durable stores make crash recovery and writer fencing
//! independent of the process that opened the window.
//!
//! Stream-backed windows use only the v1 namespace and exact-expiry lease gates.
//! Previously stored v2 windows are incompatible and are not read or migrated.
//! Each renewal retains another gate stream; lifetime storage depends on provider
//! retention and is not bounded by the active lease count.

use crate::record_store::{MAXIMUM_CAS_ATTEMPTS, MemoryRecords, next_revision};
use crate::{
    AsyncAuthorityStore, AsyncObjectStore, GenerationId, IdempotencyKey, OperationId, Workspace,
    WorkspaceError, WorkspaceId, WorkspaceRebase,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[cfg(feature = "distributed")]
use futures::StreamExt as _;

#[cfg(feature = "distributed")]
pub(crate) mod lease_path;

const STATE_VERSION: u32 = 1;

uuid_identity!(OperationLeaseId, "Stable identity of one tool lease.");

/// One active durable tool lease.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationLease {
    /// Stable lease identity.
    pub id: OperationLeaseId,
    /// Adapter-defined owner identity, normally a Codex agent and tool call.
    pub owner: String,
    /// Absolute Unix epoch expiry in milliseconds.
    pub expires_at_millis: u64,
}

/// Durable window phase.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OperationWindowPhase {
    /// No tool currently pins the mounted view.
    Idle,
    /// One or more tools share a pinned view.
    Active {
        /// Parent generation visible when the first lease opened.
        pinned_parent: GenerationId,
        /// Active leases keyed by stable identity.
        leases: BTreeMap<OperationLeaseId, OperationLease>,
        /// Newest parent generation observed while the view was pinned.
        pending_parent: Option<GenerationId>,
    },
    /// The final lease closed and one owner must complete reconciliation.
    Reconciling {
        /// Stable retry identity for reconciliation.
        ticket: OperationId,
        /// Parent generation at window entry.
        pinned_parent: GenerationId,
        /// Newest coalesced parent, if the parent advanced.
        pending_parent: Option<GenerationId>,
        /// Parent observed after this reconciliation ticket was claimed.
        subsequent_parent: Option<GenerationId>,
    },
}

/// Complete versioned durable state for one workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationWindowSnapshot {
    /// Serialization contract version.
    pub version: u32,
    /// Monotonic optimistic-concurrency revision.
    pub revision: u64,
    /// Workspace governed by this state.
    pub workspace_id: WorkspaceId,
    /// Current coordination phase.
    pub phase: OperationWindowPhase,
}

impl OperationWindowSnapshot {
    /// Validates persisted identity, version, revision and phase shape.
    #[must_use]
    pub fn is_valid_for(&self, workspace_id: WorkspaceId) -> bool {
        self.version == STATE_VERSION
            && self.workspace_id == workspace_id
            && self.revision > 0
            && snapshot_shape_is_valid(self)
    }

    fn idle(workspace_id: WorkspaceId) -> Self {
        Self {
            version: STATE_VERSION,
            revision: 0,
            workspace_id,
            phase: OperationWindowPhase::Idle,
        }
    }
}

fn snapshot_shape_is_valid(snapshot: &OperationWindowSnapshot) -> bool {
    match &snapshot.phase {
        OperationWindowPhase::Idle => true,
        OperationWindowPhase::Reconciling { ticket, .. } => ticket.into_bytes() != [0; 16],
        OperationWindowPhase::Active { leases, .. } => {
            !leases.is_empty()
                && leases.iter().all(|(lease_id, lease)| {
                    *lease_id == lease.id
                        && lease_id.into_bytes() != [0; 16]
                        && !lease.owner.trim().is_empty()
                        && lease.expires_at_millis > 0
                })
        }
    }
}

/// Lease returned to one filesystem-touching tool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationWindowLease {
    /// Workspace containing the shared mount.
    pub workspace_id: WorkspaceId,
    /// Stable lease identity.
    pub lease_id: OperationLeaseId,
    /// Parent generation pinned by the shared window.
    pub pinned_parent: GenerationId,
    /// Lease expiry copied from durable state.
    pub expires_at_millis: u64,
}

impl OperationWindowLease {
    /// Returns the authority permit that atomically admits publications made
    /// by this exact still-active lease.
    #[must_use]
    pub const fn publication_permit(&self) -> crate::PublicationPermit {
        crate::PublicationPermit::Lease {
            authority_id: crate::kernel::volume_authority_id(self.workspace_id.volume_id())
                .into_bytes(),
            workspace_id: self.workspace_id.into_bytes(),
            lease_id: self.lease_id.into_bytes(),
            expires_at_millis: self.expires_at_millis,
        }
    }
}

/// Reconciliation work claimed after a final close or expiry sweep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationWindowReconcile {
    /// Stable retry identity.
    pub ticket: OperationId,
    /// Parent generation visible when the window opened.
    pub pinned_parent: GenerationId,
    /// Newest coalesced parent generation.
    pub pending_parent: Option<GenerationId>,
}

/// Result of closing or recovering a lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationWindowFinish {
    /// Other live leases still pin the view.
    StillActive {
        /// Number of unexpired leases that remain.
        remaining: u32,
    },
    /// The caller owns the one reconciliation attempt.
    Reconcile(OperationWindowReconcile),
    /// The lease was already closed or expired.
    AlreadyClosed,
}

/// What the final close of a window owes the parent.
enum FinalClose {
    /// Reconcile what the window observed.
    Reconcile,
    /// Nothing: the fork is based on its parent's head.
    Settle,
    /// Rebase onto this parent head.
    Rebase(GenerationId),
}

/// Bounds applied when the last tool lease reconciles a fork with its parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationReconcileLimits {
    /// Maximum immutable generations inspected while finding ancestry.
    pub maximum_generations: u32,
    /// Maximum semantic changes retained during planning.
    pub maximum_changes: u32,
    /// Maximum conflicts returned to the caller.
    pub maximum_conflicts: u32,
}

impl Default for OperationReconcileLimits {
    fn default() -> Self {
        Self {
            maximum_generations: 4_096,
            maximum_changes: 65_536,
            maximum_conflicts: 1_024,
        }
    }
}

#[cfg(kani)]
#[path = "operation_window/kani_reconcile_limits_proof.rs"]
mod kani_reconcile_limits_proof;

fn validate_reconcile_limits(limits: OperationReconcileLimits) -> Result<(), WorkspaceError> {
    if limits.maximum_generations == 0
        || limits.maximum_changes == 0
        || limits.maximum_conflicts == 0
    {
        return Err(WorkspaceError::JoinLimit);
    }
    Ok(())
}

/// Workspace-aware result of closing one tool lease.
pub enum WorkspaceOperationFinish<A, O> {
    /// Other live leases still pin the shared mount.
    StillActive {
        /// Number of unexpired leases that remain.
        remaining: u32,
    },
    /// The lease was already closed or expired.
    AlreadyClosed,
    /// The final close ran the core live-rebase operation.
    Reconciled(WorkspaceRebase<A, O>),
}

/// Durable optimistic-concurrency adapter for operation-window state.
pub trait OperationWindowStore: acyclic_stream::ProviderPlatform {
    /// Adapter error.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Loads current state, returning `None` before first use.
    fn load(
        &self,
        workspace_id: WorkspaceId,
    ) -> impl Future<Output = Result<Option<OperationWindowSnapshot>, Self::Error>>
    + acyclic_stream::ProviderTask;

    /// Replaces `expected_revision` atomically. Revision zero creates state.
    fn compare_and_swap(
        &self,
        workspace_id: WorkspaceId,
        expected_revision: u64,
        replacement: OperationWindowSnapshot,
    ) -> impl Future<Output = Result<bool, Self::Error>> + acyclic_stream::ProviderTask;
}

/// Operation-window state stored beside generation authority in one Stream
/// provider. Each lease owns an immutable gate stream: opening creates tail 1,
/// while close/expiry atomically advances it to tail 2 with the window CAS.
/// A generation publication conditioned on tail 1 therefore linearizes
/// exactly against fencing.
#[derive(Clone)]
#[cfg(feature = "distributed")]
pub struct StreamOperationWindowStore<P> {
    provider: Arc<P>,
}

#[cfg(feature = "distributed")]
impl<P> StreamOperationWindowStore<P> {
    /// Binds operation windows to the exact provider used by filesystem
    /// authority publication.
    #[must_use]
    pub fn new(provider: Arc<P>) -> Self {
        Self { provider }
    }
}

/// Stream-backed operation-window failure.
#[cfg(feature = "distributed")]
#[derive(Debug, Error)]
pub enum StreamOperationWindowStoreError {
    /// The shared Stream provider rejected or could not durably resolve state.
    #[error("operation-window stream failed: {0}")]
    Stream(#[from] acyclic_stream::StreamError),
    /// Stored state is corrupt or incompatible.
    #[error("operation-window stream state is corrupt: {0}")]
    Corrupt(String),
}

#[cfg(feature = "distributed")]
impl<P: acyclic_stream::StreamProvider> OperationWindowStore for StreamOperationWindowStore<P> {
    type Error = StreamOperationWindowStoreError;

    async fn load(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<OperationWindowSnapshot>, Self::Error> {
        let path = stream_window_path(workspace_id)?;
        let tail = match self.provider.tail(path.clone()).await {
            Ok(tail) => tail,
            Err(acyclic_stream::StreamError::NotFound) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if tail == 0 {
            return Err(StreamOperationWindowStoreError::Corrupt(
                "window stream is empty".to_owned(),
            ));
        }
        let mut records = self
            .provider
            .read(acyclic_stream::ReadRequest {
                path,
                from: tail - 1,
                limit: 1,
            })
            .await?;
        let record = records.next().await.transpose()?.ok_or_else(|| {
            StreamOperationWindowStoreError::Corrupt("window stream tail has no record".to_owned())
        })?;
        let snapshot: OperationWindowSnapshot = serde_json::from_slice(&record.value)
            .map_err(|error| StreamOperationWindowStoreError::Corrupt(error.to_string()))?;
        if snapshot.revision != tail || !snapshot.is_valid_for(workspace_id) {
            return Err(StreamOperationWindowStoreError::Corrupt(
                "window revision does not match its stream tail".to_owned(),
            ));
        }
        Ok(Some(snapshot))
    }

    async fn compare_and_swap(
        &self,
        workspace_id: WorkspaceId,
        expected_revision: u64,
        replacement: OperationWindowSnapshot,
    ) -> Result<bool, Self::Error> {
        if !replacement.is_valid_for(workspace_id)
            || replacement.revision != expected_revision.saturating_add(1)
        {
            return Err(StreamOperationWindowStoreError::Corrupt(
                "replacement identity or revision is invalid".to_owned(),
            ));
        }
        let current = self.load(workspace_id).await?;
        if current.as_ref().map_or(0, |snapshot| snapshot.revision) != expected_revision {
            return Ok(false);
        }
        let before = current
            .as_ref()
            .map_or_else(BTreeMap::new, |snapshot| phase_leases(&snapshot.phase));
        let after = phase_leases(&replacement.phase);
        let state_path = stream_window_path(workspace_id)?;
        let mut conditions = vec![if expected_revision == 0 {
            acyclic_stream::CommitCondition::Absent {
                path: state_path.clone(),
            }
        } else {
            acyclic_stream::CommitCondition::Tail {
                path: state_path.clone(),
                expected: expected_revision,
            }
        }];
        let encoded = serde_json::to_vec(&replacement)
            .map_err(|error| StreamOperationWindowStoreError::Corrupt(error.to_string()))?;
        let mut mutations = vec![acyclic_stream::CommitMutation::Append {
            path: state_path,
            records: vec![encoded.into()],
        }];
        // An expiry is the admission token for this stable lease identity.
        // Closing the old token and opening its successor share the window CAS,
        // so publication can linearize only before or after the renewal.
        for (lease_id, lease) in &after {
            if let Some(previous) = before.get(lease_id) {
                if previous == lease {
                    continue;
                }
                if previous.owner != lease.owner
                    || lease.expires_at_millis <= previous.expires_at_millis
                {
                    return Err(StreamOperationWindowStoreError::Corrupt(
                        "lease replacement must retain owner and increase expiry".to_owned(),
                    ));
                }
            }
            let path = stream_lease_path(workspace_id, *lease_id, lease.expires_at_millis)?;
            conditions.push(acyclic_stream::CommitCondition::Absent { path: path.clone() });
            mutations.push(acyclic_stream::CommitMutation::Append {
                path,
                records: vec![acyclic_stream::OwnedBytes::from_static(b"active")],
            });
        }
        for (lease_id, lease) in &before {
            if after.get(lease_id) == Some(lease) {
                continue;
            }
            let path = stream_lease_path(workspace_id, *lease_id, lease.expires_at_millis)?;
            conditions.push(acyclic_stream::CommitCondition::Tail {
                path: path.clone(),
                expected: 1,
            });
            mutations.push(acyclic_stream::CommitMutation::Append {
                path,
                records: vec![acyclic_stream::OwnedBytes::from_static(b"fenced")],
            });
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"acyclic-operation-window-cas-v1\0");
        hasher.update(&workspace_id.into_bytes());
        hasher.update(&expected_revision.to_le_bytes());
        hasher.update(
            &serde_json::to_vec(&replacement)
                .map_err(|error| StreamOperationWindowStoreError::Corrupt(error.to_string()))?,
        );
        let key = acyclic_stream::IdempotencyKey::new(hasher.finalize().as_bytes().to_vec())?;
        match self
            .provider
            .commit(acyclic_stream::CommitRequest {
                conditions,
                mutations,
                idempotency_key: key,
            })
            .await?
        {
            acyclic_stream::CommitOutcome::Committed(_) => Ok(true),
            acyclic_stream::CommitOutcome::Conflict(_) => Ok(false),
        }
    }
}

#[cfg(feature = "distributed")]
fn phase_leases(phase: &OperationWindowPhase) -> BTreeMap<OperationLeaseId, OperationLease> {
    match phase {
        OperationWindowPhase::Active { leases, .. } => leases.clone(),
        OperationWindowPhase::Idle | OperationWindowPhase::Reconciling { .. } => BTreeMap::new(),
    }
}

#[cfg(feature = "distributed")]
fn stream_window_path(
    workspace_id: WorkspaceId,
) -> Result<acyclic_stream::StreamPath, acyclic_stream::StreamError> {
    acyclic_stream::StreamPath::new(format!(
        "fs/operation-windows-v1/{}/state",
        hex::encode(workspace_id.into_bytes())
    ))
}

#[cfg(feature = "distributed")]
pub(crate) fn stream_lease_path(
    workspace_id: WorkspaceId,
    lease_id: OperationLeaseId,
    expires_at_millis: u64,
) -> Result<acyclic_stream::StreamPath, acyclic_stream::StreamError> {
    let mut text = [0_u8; lease_path::MAX_ENCODED_BYTES];
    let length = lease_path::encode(workspace_id, lease_id, expires_at_millis, &mut text, None)
        .map_err(|_| acyclic_stream::StreamError::InvalidPath)?;
    acyclic_stream::StreamPath::new(
        std::str::from_utf8(
            text.get(..length)
                .ok_or(acyclic_stream::StreamError::InvalidPath)?,
        )
        .map_err(|_| acyclic_stream::StreamError::InvalidPath)?,
    )
}

/// Operation-window failure.
#[derive(Debug, Error)]
pub enum OperationWindowError<E: std::error::Error + 'static> {
    /// Durable adapter failed.
    #[error("operation-window store failed: {0}")]
    Store(E),
    /// Persisted state uses an unsupported version or identity.
    #[error("operation-window state is incompatible")]
    IncompatibleState,
    /// A reconciliation owner already fences new windows.
    #[error("workspace operation window is reconciling")]
    Reconciling,
    /// The supplied lease lifetime is empty or already expired.
    #[error("operation lease must expire after the current time")]
    InvalidExpiry,
    /// A deterministic lease identity was already bound to different inputs.
    #[error("operation lease identity is already bound to another owner or expiry")]
    LeaseIdentityConflict,
    /// The supplied lease was closed, expired, or superseded by a renewal.
    #[error("operation lease is no longer active")]
    StaleLease,
    /// Optimistic-concurrency contention exceeded the bounded retry policy.
    #[error("operation-window state remained contended")]
    Contended,
    /// Reconciliation completion used a stale ticket.
    #[error("operation-window reconciliation ticket is stale")]
    StaleTicket,
    /// The core workspace reconciliation failed.
    #[error("workspace reconciliation failed: {0}")]
    Workspace(#[source] WorkspaceError),
}

include!("operation_window/coordinator.rs");

/// Process-local adapter used by tests and embedded single-process callers.
#[derive(Clone, Default)]
pub struct MemoryOperationWindowStore {
    states: Arc<MemoryRecords<WorkspaceId, OperationWindowSnapshot>>,
}

impl MemoryOperationWindowStore {
    /// Creates an empty state adapter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Process-local store synchronization failure.
#[derive(Debug, Error)]
#[error("operation-window memory store is unavailable")]
pub struct MemoryOperationWindowStoreError;

impl OperationWindowStore for MemoryOperationWindowStore {
    type Error = MemoryOperationWindowStoreError;

    async fn load(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<OperationWindowSnapshot>, Self::Error> {
        self.states
            .load(&workspace_id)
            .map_err(|_| MemoryOperationWindowStoreError)
    }

    async fn compare_and_swap(
        &self,
        workspace_id: WorkspaceId,
        expected_revision: u64,
        replacement: OperationWindowSnapshot,
    ) -> Result<bool, Self::Error> {
        self.states
            .compare_and_swap(workspace_id, expected_revision, replacement)
            .map_err(|_| MemoryOperationWindowStoreError)
    }
}

#[cfg(test)]
#[path = "operation_window/tests/mod.rs"]
mod tests;

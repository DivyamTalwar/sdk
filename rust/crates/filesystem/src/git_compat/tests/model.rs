use super::*;

#[test]
fn concurrent_branch_registration_preserves_both_atomic_updates() {
    // Two writers, one update each: exhaust every scheduling and CAS ordering
    // through the real repository and instrumented MemoryRecords mutex.
    loom::model(|| {
        let repository = loom::sync::Arc::new(GitCompatRepository::new(
            WorkspaceId::from_bytes([0; 16]),
            MemoryGitCompatStore::default(),
        ));
        let mut writers = Vec::new();
        for (name, identity) in [("A", [1; 16]), ("B", [2; 16])] {
            let repository = repository.clone();
            writers.push(loom::thread::spawn(move || {
                futures::executor::block_on(repository.register_branch_workspace(
                    name,
                    WorkspaceId::from_bytes(identity),
                    None,
                    false,
                ))
                .unwrap();
            }));
        }
        for writer in writers {
            writer.join().unwrap();
        }
        let state =
            futures::executor::block_on(repository.store.load(WorkspaceId::from_bytes([0; 16])))
                .unwrap()
                .unwrap();
        assert_eq!(state.branches.len(), 3);
        assert_eq!(
            state.branches["A"].workspace_id,
            WorkspaceId::from_bytes([1; 16])
        );
        assert_eq!(
            state.branches["B"].workspace_id,
            WorkspaceId::from_bytes([2; 16])
        );
        assert_eq!(state.revision, 2);
        assert!(validate_git_state(&state).is_ok());
    });
}

#[test]
fn concurrent_renewal_fences_the_superseded_lease() {
    use crate::{
        MemoryOperationWindowStore, OperationLeaseId, OperationWindowCoordinator,
        OperationWindowError, OperationWindowPhase,
    };

    // One lease, two renewals: run the real coordinator's snapshot/CAS/retry
    // path in every interleaving, with deterministic time and identities.
    loom::model(|| {
        let coordinator = loom::sync::Arc::new(OperationWindowCoordinator::new(
            MemoryOperationWindowStore::new(),
        ));
        let workspace = WorkspaceId::from_bytes([0; 16]);
        let lease = futures::executor::block_on(coordinator.begin_with_lease_id(
            workspace,
            generation(1),
            "owner",
            1,
            10,
            OperationLeaseId::from_bytes([1; 16]),
        ))
        .unwrap();
        let mut renewals = Vec::new();
        for expiry in [20, 30] {
            let coordinator = coordinator.clone();
            let lease = lease.clone();
            renewals.push(loom::thread::spawn(move || {
                futures::executor::block_on(coordinator.renew(&lease, 5, expiry))
            }));
        }
        let mut successful = Vec::new();
        for renewal in renewals {
            match renewal.join().unwrap() {
                Ok(lease) => successful.push(lease),
                Err(OperationWindowError::StaleLease) => {}
                other => panic!("unexpected renewal: {other:?}"),
            }
        }
        assert_eq!(successful.len(), 1);
        let snapshot = futures::executor::block_on(coordinator.snapshot(workspace)).unwrap();
        let OperationWindowPhase::Active {
            leases,
            pinned_parent,
            ..
        } = snapshot.phase
        else {
            panic!("renewal lost the active window");
        };
        assert_eq!(pinned_parent, generation(1));
        assert_eq!(leases.len(), 1);
        assert_eq!(
            leases[&lease.lease_id].expires_at_millis,
            successful[0].expires_at_millis
        );
        assert!(matches!(
            futures::executor::block_on(coordinator.renew(&lease, 5, 40)),
            Err(OperationWindowError::StaleLease)
        ));
    });
}

use super::*;
use crate::operation_window::lease_path::{MAX_ENCODED_BYTES, encode};
use crate::{OperationLeaseId, WorkspaceId};

const BUDGET: WorkBudget = crate::performance::BOUNDARY_BUDGET;

fn legacy_path(
    workspace: WorkspaceId,
    lease: OperationLeaseId,
    expiry: u64,
    work: &mut AuthorityWork<'_>,
) -> Result<StreamPath, AuthorityStoreError> {
    let mut text = [0; acyclic_stream::MAX_PATH_BYTES];
    let length = encode(
        workspace,
        lease,
        expiry,
        text.first_chunk_mut::<MAX_ENCODED_BYTES>().unwrap(),
        Some(work),
    )?;
    work.own_path(std::str::from_utf8(&text[..length]).unwrap())
}

#[test]
fn decimal_boundaries_and_maximum_expiry_preserve_paths_and_native_custody() {
    crate::initialize_test_native_io();
    assert_eq!(MAX_ENCODED_BYTES, 117);
    let workspace = WorkspaceId::from_bytes([3; 16]);
    let lease = OperationLeaseId::from_bytes([5; 16]);
    let cancellation = CancellationToken::new();
    let mut expiries = std::collections::BTreeSet::from([0, u64::MAX]);
    for exponent in 0..20 {
        let boundary = 10_u64.pow(exponent);
        expiries.extend([boundary - 1, boundary, boundary + 1]);
    }
    for expiry in expiries {
        let before = crate::NativeMaterializationWork::new(BUDGET, &cancellation).unwrap();
        let after = crate::NativeMaterializationWork::new(BUDGET, &cancellation).unwrap();
        let baseline = after.live_allocation_bytes();
        let old = legacy_path(
            workspace,
            lease,
            expiry,
            &mut AuthorityWork::native(&before, &cancellation),
        )
        .unwrap();
        let actual = AuthorityWork::native(&after, &cancellation)
            .lease_path(workspace, lease, expiry)
            .unwrap();
        let expected = format!(
            "fs/operation-windows-v1/{}/leases/{}/{expiry}",
            hex::encode(workspace.into_bytes()),
            hex::encode(lease.into_bytes())
        );
        assert_eq!(actual.as_str(), expected);
        assert_eq!(actual, old);
        assert_eq!(after.receipt(), before.receipt());
        assert!(after.live_allocation_bytes() > baseline);
        drop(actual);
        assert_eq!(after.live_allocation_bytes(), baseline);
        after.receipt().verify(BUDGET).unwrap();
    }
}

proptest::proptest! {
    // The exact scratch bound preserves every canonical byte, admission error
    // and receipt field for arbitrary identities, expiries and budget prefixes.
    #[test]
    fn exact_lease_scratch_preserves_bytes_errors_and_charges(
        workspace in proptest::prelude::any::<[u8; 16]>(),
        lease in proptest::prelude::any::<[u8; 16]>(),
        expiry in proptest::prelude::any::<u64>(),
        limit in 0u8..7,
        cancelled in proptest::prelude::any::<bool>(),
    ) {
        let workspace = WorkspaceId::from_bytes(workspace);
        let lease = OperationLeaseId::from_bytes(lease);
        let expected = format!("fs/operation-windows-v1/{}/leases/{}/{expiry}", hex::encode(workspace.into_bytes()), hex::encode(lease.into_bytes()));
        let length = expected.len() as u64;
        let prefix = b"fs/operation-windows-v1/".len() as u64 + b"/leases/".len() as u64;
        let mut budget = BUDGET;
        match limit {
            1 => budget.bytes_encoded = length - 1,
            2 => budget.bytes_copied = prefix - 1,
            3 => budget.allocation_operations = 0,
            4 => budget.peak_allocation_bytes = 0,
            5 => budget.bytes_copied = prefix + length - 1,
            6 => {
                budget.bytes_encoded = length;
                budget.bytes_copied = prefix + length;
                budget.peak_allocation_bytes = length;
            }
            _ => {}
        }
        let cancellation = CancellationToken::new();
        if cancelled { cancellation.cancel(); }
        let mut old = AuthorityWork::new(budget, &cancellation);
        let legacy = legacy_path(workspace, lease, expiry, &mut old);
        let mut work = AuthorityWork::new(budget, &cancellation);
        let actual = work.lease_path(workspace, lease, expiry);
        match (actual, legacy) {
            (Ok(actual), Ok(legacy)) => {
                proptest::prop_assert_eq!(actual.as_str(), legacy.as_str());
                proptest::prop_assert_eq!(actual.as_str(), expected.as_str());
                let unmetered = crate::operation_window::stream_lease_path(workspace, lease, expiry).unwrap();
                proptest::prop_assert_eq!(actual, unmetered);
            }
            (Err(actual), Err(legacy)) => proptest::prop_assert_eq!(actual.to_string(), legacy.to_string()),
            (actual, legacy) => proptest::prop_assert!(false, "actual={actual:?}, legacy={legacy:?}"),
        }
        proptest::prop_assert_eq!(work.spent(), old.spent());
        let expected_work = if cancelled || matches!(limit, 1 | 2) {
            WorkCounters::UNCHARGED
        } else {
            let completed_path = matches!(limit, 0 | 6);
            WorkCounters {
                bytes_encoded: length,
                bytes_copied: prefix + if completed_path { length } else { 0 },
                allocation_operations: u64::from(completed_path),
                peak_allocation_bytes: if completed_path { length } else { 0 },
                ..WorkCounters::UNCHARGED
            }
        };
        proptest::prop_assert_eq!(work.spent(), expected_work);
    }
}

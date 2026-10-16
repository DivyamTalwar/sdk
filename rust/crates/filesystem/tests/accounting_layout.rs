//! Actual allocator requests and admission boundaries for the original ledger.
#![cfg(not(target_arch = "wasm32"))]

use acyclic_fs::{CancellationToken, NativeMaterializationWork, WorkBudget, WorkCounters};
fn measured<T>(operation: impl FnOnce() -> T) -> (T, allocation_counter::AllocationInfo) {
    let mut result = None;
    let allocations = allocation_counter::measure(|| result = Some(operation()));
    (result.unwrap(), allocations)
}

#[test]
fn original_ledger_records_the_actual_allocator_layout() {
    let cancellation = CancellationToken::new();
    let (work, allocations) = measured(|| {
        NativeMaterializationWork::new(acyclic_fs::performance::BOUNDARY_BUDGET, &cancellation)
            .unwrap()
    });
    assert_eq!(allocations.count_total, 1);
    assert_eq!(allocations.count_current, 1);
    assert_eq!(allocations.bytes_current as u64, allocations.bytes_total);
    assert_eq!(allocations.bytes_max, allocations.bytes_total);
    eprintln!("ledger allocator: {allocations:?}");
    assert_eq!(
        allocations.bytes_total,
        NativeMaterializationWork::accounting_state_bytes()
    );
    assert_eq!(work.live_allocation_bytes(), allocations.bytes_total);
    assert_eq!(
        work.receipt().peak_allocation_bytes,
        allocations.bytes_total
    );
    assert_eq!(work.receipt().allocation_operations, 1);
}

#[test]
fn denied_ledger_does_not_allocate_its_arc() {
    let cancellation = CancellationToken::new();
    let (probe, allocations) = measured(|| {
        NativeMaterializationWork::new(acyclic_fs::performance::BOUNDARY_BUDGET, &cancellation)
            .unwrap()
    });
    let required = allocations.bytes_total;
    drop(probe);
    for ceiling in 0..=required {
        let (result, allocations) = measured(|| {
            NativeMaterializationWork::new(
                WorkBudget {
                    peak_allocation_bytes: ceiling,
                    ..acyclic_fs::performance::BOUNDARY_BUDGET
                },
                &cancellation,
            )
        });
        if ceiling == required {
            let work = result.unwrap();
            assert_eq!(allocations.count_total, 1);
            assert_eq!(allocations.bytes_total, required);
            assert_eq!(work.live_allocation_bytes(), required);
            assert_eq!(work.receipt().peak_allocation_bytes, required);
            assert_eq!(work.receipt().allocation_operations, 1);
            continue;
        }
        let (_, work) = result.unwrap_err();
        assert_eq!(*work, WorkCounters::UNCHARGED);
        // The typed error owns one receipt; the admitted ledger Arc is absent.
        assert_eq!(allocations.count_total, 1);
        assert_eq!(
            allocations.bytes_total,
            std::mem::size_of::<WorkCounters>() as u64
        );
    }
}

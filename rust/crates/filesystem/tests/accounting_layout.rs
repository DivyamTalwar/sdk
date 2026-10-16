#![cfg(not(target_arch = "wasm32"))]

use acyclic_fs::{CancellationToken, NativeMaterializationWork, WorkBudget, WorkCounters};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Default)]
struct Allocations {
    count: usize,
    bytes: usize,
}
thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<Allocations> = const { Cell::new(Allocations { count: 0, bytes: 0 }) };
}

struct CountingAllocator;

fn record(bytes: usize) {
    if ACTIVE.try_with(Cell::get).unwrap_or(false) {
        ALLOCATIONS.with(|counter| {
            let mut allocations = counter.get();
            allocations.count += 1;
            allocations.bytes += bytes;
            counter.set(allocations);
        });
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measured<T>(operation: impl FnOnce() -> T) -> (T, Allocations) {
    ALLOCATIONS.set(Allocations::default());
    ACTIVE.set(true);
    let result = operation();
    ACTIVE.set(false);
    (result, ALLOCATIONS.get())
}

#[test]
fn original_ledger_records_the_actual_allocator_layout() {
    let cancellation = CancellationToken::new();
    let (work, allocations) = measured(|| {
        NativeMaterializationWork::new(acyclic_fs::performance::BOUNDARY_BUDGET, &cancellation)
            .unwrap()
    });
    assert_eq!(allocations.count, 1);
    assert_eq!(
        allocations.bytes as u64,
        NativeMaterializationWork::accounting_state_bytes()
    );
    assert_eq!(work.live_allocation_bytes(), allocations.bytes as u64);
    assert_eq!(
        work.receipt().peak_allocation_bytes,
        allocations.bytes as u64
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
    let required = allocations.bytes as u64;
    drop(probe);
    for ceiling in [0, required - 1] {
        let (result, allocations) = measured(|| {
            NativeMaterializationWork::new(
                WorkBudget {
                    peak_allocation_bytes: ceiling,
                    ..acyclic_fs::performance::BOUNDARY_BUDGET
                },
                &cancellation,
            )
        });
        let (_, work) = result.unwrap_err();
        assert_eq!(*work, WorkCounters::UNCHARGED);
        // The typed error owns one receipt; the admitted ledger Arc is absent.
        assert_eq!(allocations.count, 1);
        assert_eq!(allocations.bytes, std::mem::size_of::<WorkCounters>());
    }
}

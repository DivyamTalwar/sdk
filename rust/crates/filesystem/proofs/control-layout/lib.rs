#[path = "../../src/materializer/arc_layout.rs"]
mod arc_layout;
#[path = "../../src/performance.rs"]
pub mod performance;

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(1)]
fn complete_arc_layout_and_admission() {
    use performance::{WorkBudget, WorkCounters};
    use std::alloc::Layout;

    let size: usize = kani::any();
    let shift: u8 = kani::any();
    kani::assume(u32::from(shift) < usize::BITS);
    let Ok(payload) = Layout::from_size_align(size, 1_usize << shift) else {
        return;
    };
    let align = payload.align().max(std::mem::align_of::<usize>());
    let mask = align - 1;
    let prefix = (2 * std::mem::size_of::<usize>() + mask) & !mask;
    let expected = (prefix + size + mask) & !mask;
    let result = arc_layout::allocation_bytes(payload);
    assert_eq!(result.is_ok(), expected <= isize::MAX as usize);
    let Ok(bytes) = result else { return };
    assert_eq!(bytes, expected);

    let budget = WorkBudget {
        peak_allocation_bytes: kani::any(),
        allocation_operations: kani::any(),
        ..WorkBudget::UNBOUNDED
    };
    let mut work = WorkCounters::UNCHARGED;
    let result = work.charge_allocation(1, bytes as u64, &budget);
    let admitted =
        budget.allocation_operations >= 1 && budget.peak_allocation_bytes >= bytes as u64;
    assert_eq!(result.is_ok(), admitted);
    assert_eq!(
        work,
        if admitted {
            WorkCounters {
                allocation_operations: 1,
                peak_allocation_bytes: bytes as u64,
                ..WorkCounters::UNCHARGED
            }
        } else {
            WorkCounters::UNCHARGED
        }
    );
}

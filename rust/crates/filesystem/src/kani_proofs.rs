use crate::exact_u32_from_f64;

#[kani::proof]
#[kani::unwind(1)]
fn exact_u32_from_f64_all_bits() {
    let bits: u64 = kani::any();
    let number = f64::from_bits(bits);
    let admissible = number.is_finite()
        && number.fract() == 0.0
        && number >= 0.0
        && number <= f64::from(u32::MAX);

    match exact_u32_from_f64(number) {
        Ok(value) => {
            assert!(admissible);
            assert_eq!(f64::from(value), number);
        }
        Err(_) => assert!(!admissible),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[kani::proof]
#[kani::unwind(1)]
fn original_ledger_admission_funds_its_entire_arc() {
    use crate::{CancellationToken, NativeMaterializationWork, WorkBudget};
    use std::alloc::Layout;
    use std::sync::Arc;

    let allocation = Layout::new::<[usize; 2]>()
        .extend(Layout::new::<crate::demand::NativeDemandReadMeter>())
        .unwrap()
        .0
        .pad_to_align()
        .size() as u64;
    let ceiling: u64 = kani::any();
    let budget = WorkBudget {
        peak_allocation_bytes: ceiling,
        ..crate::performance::BOUNDARY_BUDGET
    };
    let cancellation = CancellationToken::new();
    match NativeMaterializationWork::new(budget, &cancellation) {
        Ok(work) => {
            assert!(ceiling >= allocation);
            let meter = Arc::try_unwrap(work.state).unwrap();
            let state = meter.0.into_inner().unwrap();
            assert_eq!(state.work.peak_allocation_bytes, allocation);
            assert_eq!(state.work.allocation_operations, 1);
        }
        Err((_, work)) => {
            assert!(ceiling < allocation);
            assert_eq!(*work, crate::WorkCounters::UNCHARGED);
        }
    }
}

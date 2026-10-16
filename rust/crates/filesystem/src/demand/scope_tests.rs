use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

proptest::proptest! {
    // Property: arbitrary nested scopes restore the caller's value on both
    // return and unwind, without undoing work completed inside the scope.
    #[test]
    fn nested_scopes_restore_policy_and_preserve_completed_work(
        original in proptest::bool::ANY,
        frames in proptest::collection::vec((proptest::bool::ANY, proptest::num::u8::ANY), 1..33),
        unwind in proptest::bool::ANY,
    ) {
        let cancellation = CancellationToken::new();
        let mut work = DemandWork::new(crate::performance::BOUNDARY_BUDGET, &cancellation);
        work.recheck_items = original;
        let result = catch_unwind(AssertUnwindSafe(|| nested(&mut work, &frames, unwind)));
        proptest::prop_assert_eq!(result.is_err(), unwind);
        proptest::prop_assert_eq!(work.recheck_items, original);
        proptest::prop_assert_eq!(work.work.bytes_copied, frames.iter().map(|(_, bytes)| u64::from(*bytes)).sum::<u64>());
    }
}

fn nested(work: &mut DemandWork, frames: &[(bool, u8)], unwind: bool) {
    let Some((&(policy, bytes), remaining)) = frames.split_first() else {
        if unwind {
            std::panic::panic_any(());
        }
        return;
    };
    let original = work.recheck_items;
    work.scope(
        |work| &mut work.recheck_items,
        policy,
        |work| {
            assert_eq!(work.recheck_items, policy);
            work.work.bytes_copied += u64::from(bytes);
            nested(work, remaining, unwind);
            assert_eq!(work.recheck_items, policy);
        },
    );
    assert_eq!(work.recheck_items, original);
}

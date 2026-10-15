use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn arbitrary_replacements_preserve_bounds_and_fail_without_changing_the_balance(
        maximum in 1usize..4096,
        replacements in prop::collection::vec((any::<u16>(), any::<u16>(), any::<u16>(), any::<u16>(), any::<bool>()), 0..64),
    ) {
        let mut budget = Budget::new(maximum);
        for (remove_bytes, remove_items, add_bytes, add_items, overflow) in replacements {
            let before = budget.used();
            let old = Size {
                bytes: usize::from(remove_bytes) % (before.0 + 2),
                items: usize::from(remove_items) % (before.1 + 2),
            };
            let new = Size {
                bytes: if overflow { usize::MAX } else { usize::from(add_bytes) % (maximum + 1) },
                items: usize::from(add_items) % (maximum.min(1024) + 1),
            };
            let result = budget.replace(old, new);
            let after = budget.used();
            let bytes = before.0 as i128 - old.bytes as i128 + new.bytes as i128;
            let items = before.1 as i128 - old.items as i128 + new.items as i128;
            let admissible = old.bytes <= before.0 && old.items <= before.1
                && bytes <= maximum as i128 && items <= maximum.min(1024) as i128;
            prop_assert_eq!(result.is_ok(), admissible);
            prop_assert!(after.0 <= maximum && after.1 <= maximum.min(1024));
            if result.is_err() {
                prop_assert_eq!(after, before);
            } else {
                prop_assert_eq!(after.0 as i128 + old.bytes as i128, before.0 as i128 + new.bytes as i128);
                prop_assert_eq!(after.1 as i128 + old.items as i128, before.1 as i128 + new.items as i128);
            }
        }
    }
}

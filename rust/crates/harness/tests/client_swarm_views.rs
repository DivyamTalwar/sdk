//! The reducer is exercised through actual volume writes and platform frames.
#[path = "../../harness-fuzz/support/swarm_views.rs"]
mod oracle;

#[path = "client_swarm_views_follow.rs"]
mod follow;

#[path = "client_swarm_views_retention.rs"]
mod retention;

#[test]
fn standalone_watermarks_and_same_cut_status_keys_survive_promotion() {
    oracle::exercise(b"live text", false);
}

#[test]
fn negative_control_dropping_standalone_watermarks_is_detected() {
    assert!(std::panic::catch_unwind(|| oracle::exercise(b"live text", true)).is_err());
}

proptest::proptest! {
    #[test]
    fn selected_files_and_overview_are_independent_folds(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..128)) {
        oracle::exercise(&bytes, false);
    }
}

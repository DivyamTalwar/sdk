//! Exhaustive model of the production usage calibration.
#![cfg(not(target_arch = "wasm32"))]
use acyclic_harness_providers::Adapter;
use loom::sync::{self, Arc};
#[path = "../src/accounting.rs"]
mod accounting;

#[test]
fn concurrent_calibration_preserves_the_dense_minimum_without_torn_reads() {
    loom::model(|| {
        let estimator = Arc::new(accounting::TokenEstimator::new(Adapter::Responses));
        let dense = estimator.clone();
        let dense = loom::thread::spawn(move || {
            dense.observe(144, &serde_json::json!({"usage":{"input_tokens":160}}));
        });
        let sparse = estimator.clone();
        let sparse = loom::thread::spawn(move || {
            sparse.observe(1000, &serde_json::json!({"usage":{"input_tokens":1}}));
        });
        assert!((16..=40).contains(&estimator.tokens(32).unwrap()));
        dense.join().unwrap();
        sparse.join().unwrap();
        assert_eq!(estimator.tokens(32), Some(40));
    });
}

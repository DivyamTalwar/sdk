//! Exhaustive models of production calibration and cancellation.
#![cfg(not(target_arch = "wasm32"))]

// Compile the production algorithms against Loom's synchronization primitives.
// The library and its ordinary fixture suites always use std::sync, including
// --all-features builds; only this test target substitutes the model runtime.
use acyclic_harness_providers::Adapter;
use loom::sync;
#[path = "../src/accounting.rs"]
mod accounting;
#[path = "../src/cancellation.rs"]
mod cancellation;

use cancellation::Cancellation;
use loom::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Wake, Waker};

struct Waiter {
    cancellation: Arc<Cancellation>,
    woken: AtomicBool,
}
impl Wake for Waiter {
    fn wake(self: std::sync::Arc<Self>) {
        // Reentrant wakeups must never run under the cancellation lock.
        let waker = Waker::from(self.clone());
        assert!(
            self.cancellation
                .poll(&Context::from_waker(&waker))
                .is_ready()
        );
        self.woken.store(true, Ordering::Release);
    }
}
impl Drop for Waiter {
    fn drop(&mut self) {
        // Dropping a replaced waker is also an application callback.
        self.cancellation.cancel();
    }
}

#[test]
fn registration_and_cancellation_have_no_lost_wakeup_or_lock_cycle() {
    loom::model(|| {
        let cancellation = Arc::new(Cancellation::default());
        let waiter = std::sync::Arc::new(Waiter {
            cancellation: cancellation.clone(),
            woken: AtomicBool::new(false),
        });
        let canceller = cancellation.clone();
        let other = loom::thread::spawn(move || {
            canceller.cancel();
            canceller.cancel();
        });
        let waker = Waker::from(waiter.clone());
        let ready = cancellation.poll(&Context::from_waker(&waker)).is_ready();
        other.join().unwrap();
        assert!(ready || waiter.woken.load(Ordering::Acquire));
        assert!(cancellation.poll(&Context::from_waker(&waker)).is_ready());
    });
}

#[test]
fn replacing_a_waiter_drops_its_callback_after_unlocking() {
    loom::model(|| {
        let cancellation = Arc::new(Cancellation::default());
        for _ in 0..2 {
            let waker = Waker::from(std::sync::Arc::new(Waiter {
                cancellation: cancellation.clone(),
                woken: AtomicBool::new(false),
            }));
            assert!(cancellation.poll(&Context::from_waker(&waker)).is_pending());
        }
        cancellation.cancel();
    });
}

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

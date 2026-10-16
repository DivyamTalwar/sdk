// Run from the repository root:
// RUSTFLAGS='--cfg acyclic_sdk_docs_loom' cargo test --manifest-path rust/verification/sdk-fixtures/Cargo.toml --locked --test fixture-model
#![cfg(acyclic_sdk_docs_loom)]

use loom::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use sdk_fixture_verification::get_or_try_init;

#[path = "../../crates/sdk-generation/src/parallel.rs"]
mod parallel;

fn joined_workers(broken: bool) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(move || {
        let completed = Arc::new(AtomicUsize::new(0));
        let state = Arc::clone(&completed);
        let operation = move |index| {
            state.fetch_or(1usize << index, Ordering::SeqCst);
            if index == 0 {
                Err(index)
            } else {
                Ok(index)
            }
        };
        if broken {
            // A failed worker must never detach the remaining compiler.
            completed.fetch_or(1, Ordering::SeqCst);
            drop(loom::thread::spawn(move || operation(1)));
        } else {
            assert_eq!(parallel::map(vec![0, 1], operation), vec![Err(0), Ok(1)]);
        }
        assert_eq!(
            completed.load(Ordering::SeqCst),
            3,
            "worker escaped publication barrier"
        );
    });
}

#[test]
fn all_owner_compilers_join_before_errors_or_publication() {
    joined_workers(false);
}

#[test]
#[should_panic(expected = "worker escaped publication barrier")]
fn negative_control_detects_detached_owner_compiler() {
    joined_workers(true);
}

#[path = "../../crates/sdk-generation/src/shared_cache.rs"]
mod shared_cache;

fn model(broken: bool) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(move || {
        let slot = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let jobs = [0, 1].map(|_| {
            let slot = Arc::clone(&slot);
            let calls = Arc::clone(&calls);
            loom::thread::spawn(move || {
                let initialize = || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(vec![1, 2])
                };
                if broken {
                    // Deliberately release the slot while constructing the package.
                    if let Some(value) = slot.lock().unwrap().as_ref() {
                        return Arc::clone(value);
                    }
                    let value = Arc::new(initialize().unwrap());
                    *slot.lock().unwrap() = Some(Arc::clone(&value));
                    value
                } else {
                    get_or_try_init(&slot, initialize).unwrap()
                }
            })
        });
        let [a, b] = jobs.map(|job| job.join().unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "package initialized twice");
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.as_slice(), &[1, 2]);
    });
}

#[test]
fn package_is_initialized_once_and_published_whole() {
    model(false);
}

#[test]
#[should_panic(expected = "package initialized twice")]
fn model_detects_initialization_outside_the_lock() {
    model(true);
}
#[test]
fn failed_initialization_leaves_the_slot_retryable() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(|| {
        let slot = Arc::new(Mutex::new(None));
        let calls = Arc::new(AtomicUsize::new(0));
        let jobs = [0, 1].map(|_| {
            let slot = Arc::clone(&slot);
            let calls = Arc::clone(&calls);
            loom::thread::spawn(move || {
                get_or_try_init(&slot, || {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(())
                    } else {
                        Ok(vec![1, 2])
                    }
                })
            })
        });
        let results = jobs.map(|job| job.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().as_slice(), &[1, 2]);
    });
}

#[test]
fn producer_then_cfg_slots_publish_without_nested_locks() {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(|| {
        let producer = Arc::new(Mutex::new(None));
        let cfgs = Arc::new(Mutex::new(None));
        let calls = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let jobs = [0, 1].map(|_| {
            let producer = Arc::clone(&producer);
            let cfgs = Arc::clone(&cfgs);
            let calls = Arc::clone(&calls);
            loom::thread::spawn(move || {
                let producer = get_or_try_init(&producer, || {
                    calls[0].fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(1)
                })
                .unwrap();
                let cfgs = get_or_try_init(&cfgs, || {
                    calls[1].fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(vec![*producer, 2])
                })
                .unwrap();
                assert_eq!(cfgs.as_slice(), &[1, 2]);
            })
        });
        for job in jobs {
            job.join().unwrap();
        }
        for calls in calls.iter() {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    });
}

fn shared_identity_model(broken: bool, fail_first: bool) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(2);
    builder.check(move || {
        let lock = Arc::new(shared_cache::Lock::new(()));
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let jobs = [0, 1].map(|_| {
            let lock = Arc::clone(&lock);
            let bytes = Arc::clone(&bytes);
            let calls = Arc::clone(&calls);
            loom::thread::spawn(move || {
                let read = || {
                    let bytes = bytes.lock().unwrap();
                    Ok((bytes.as_slice() == [1, 2]).then(|| bytes.clone()))
                };
                let compute = || {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    if fail_first && call == 0 {
                        return Err(std::io::Error::other("unavailable"));
                    }
                    Ok(vec![1, 2])
                };
                let publish = |value: &Vec<u8>| {
                    bytes.lock().unwrap().push(value[0]);
                    loom::thread::yield_now();
                    bytes.lock().unwrap().push(value[1]);
                    Ok(())
                };
                if broken {
                    shared_cache::without_lock(read, compute, publish)
                } else {
                    shared_cache::get_or_compute(&lock, read, compute, publish)
                }
            })
        });
        let results = jobs.map(|job| job.join().unwrap());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if fail_first { 2 } else { 1 },
            "identity cache invariant: duplicate computation"
        );
        assert_eq!(
            results.iter().filter(|result| result.is_err()).count(),
            usize::from(fail_first)
        );
        for value in results.into_iter().flatten() {
            assert_eq!(value, [1, 2], "identity cache invariant: torn publication");
        }
        assert_eq!(
            *bytes.lock().unwrap(),
            [1, 2],
            "identity cache invariant: torn stored identity"
        );
    });
}

#[test]
fn shared_identity_is_computed_once_and_published_whole() {
    shared_identity_model(false, false);
}

#[test]
#[should_panic(expected = "identity cache invariant")]
fn shared_identity_model_detects_omitted_process_lock() {
    shared_identity_model(true, false);
}

#[test]
fn failed_shared_computation_releases_the_lock_for_retry() {
    shared_identity_model(false, true);
}

#[test]
#[should_panic(expected = "nested identity cache lock")]
fn shared_identity_lock_order_checker_rejects_nesting() {
    loom::model(|| {
        let a = shared_cache::Lock::new(());
        let b = shared_cache::Lock::new(());
        let _: std::io::Result<()> = shared_cache::get_or_compute(
            &a,
            || {
                shared_cache::get_or_compute(&b, || Ok(None), || Ok(()), |_| Ok(()))?;
                Ok(None)
            },
            || Ok(()),
            |_| Ok(()),
        );
    });
}

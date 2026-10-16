mod refresh {
    include!("../../src/refresh.rs");

    // Mutation: release ownership before the operation's I/O.
    pub async fn released_early<T>(
        lock: &crate::sync::AsyncMutex<()>,
        operation: impl std::future::Future<Output = T>,
    ) -> T {
        drop(lock.lock().await);
        operation.await
    }
}

use crate::sync::{
    Arc, AsyncMutex,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

fn complete<T>(operation: impl Future<Output = T>) -> T {
    // Loom's instrumented mutex suspends the modeled thread during acquisition.
    // A host-thread-local executor would incorrectly treat that as reentrancy.
    let mut operation = std::pin::pin!(operation);
    match operation
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("unexpected suspension outside the modeled mutex"),
    }
}

fn exclusion_model(broken: bool) {
    loom::model(move || {
        let lock = Arc::new(AsyncMutex::new(()));
        let active = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for fail in [false, true] {
            let (lock, active) = (lock.clone(), active.clone());
            tasks.push(loom::thread::spawn(move || {
                let operation = async {
                    assert_eq!(
                        active.fetch_add(1, Ordering::SeqCst),
                        0,
                        "refresh ownership overlapped"
                    );
                    // Model an I/O suspension while the production guard owns credentials.
                    loom::thread::yield_now();
                    assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                    if fail { Err(()) } else { Ok(()) }
                };
                let result = if broken {
                    complete(refresh::released_early(&lock, operation))
                } else {
                    complete(refresh::serialized(&lock, operation))
                };
                assert_eq!(result.is_err(), fail);
            }));
        }
        for task in tasks {
            task.join().unwrap();
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(lock.try_lock().is_ok());
    });
}

#[test]
fn refresh_retains_exclusive_ownership_across_io_and_error_returns() {
    exclusion_model(false);
}

#[test]
#[should_panic(expected = "refresh ownership overlapped")]
fn negative_control_detects_a_guard_released_before_io() {
    exclusion_model(true);
}

fn deduplicate_model(broken: bool) {
    loom::model(move || {
        let lock = Arc::new(AsyncMutex::new(()));
        // Model the token endpoint and credential file at their I/O boundary:
        // version 0 is expired; version 1 is the successfully published bearer.
        let version = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let (lock, version, requests) = (lock.clone(), version.clone(), requests.clone());
            tasks.push(loom::thread::spawn(move || {
                let operation = async {
                    if version.load(Ordering::SeqCst) == 0 {
                        requests.fetch_add(1, Ordering::SeqCst);
                        loom::thread::yield_now();
                        version.store(1, Ordering::SeqCst);
                    }
                    version.load(Ordering::SeqCst)
                };
                let bearer = if broken {
                    complete(refresh::released_early(&lock, operation))
                } else {
                    complete(refresh::serialized(&lock, operation))
                };
                assert_eq!(bearer, 1);
            }));
        }
        for task in tasks {
            task.join().unwrap();
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1, "two refreshes raced");
    });
}

#[test]
fn expired_parallel_requests_share_one_refresh_and_reload_the_published_bearer() {
    deduplicate_model(false);
}

#[test]
#[should_panic(expected = "two refreshes raced")]
fn negative_control_detects_duplicate_token_requests_without_refresh_ownership() {
    deduplicate_model(true);
}

#[test]
fn cancelling_the_holder_releases_the_guard_and_wakes_the_waiter() {
    loom::model(|| {
        let lock = Arc::new(AsyncMutex::new(()));
        let holder_lock = lock.clone();
        let holder = loom::thread::spawn(move || {
            let mut operation = Box::pin(refresh::serialized(
                &holder_lock,
                std::future::pending::<()>(),
            ));
            assert_eq!(
                operation
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            );
            loom::thread::yield_now();
            drop(operation);
        });
        let waiter_lock = lock.clone();
        let waiter = loom::thread::spawn(move || {
            complete(refresh::serialized(&waiter_lock, async {}));
        });
        holder.join().unwrap();
        waiter.join().unwrap();
        assert!(lock.try_lock().is_ok());
    });
}

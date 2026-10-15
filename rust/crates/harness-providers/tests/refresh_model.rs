//! Exhaustive async schedules of the production refresh guard using Tokio's mutex.
#![cfg(not(target_arch = "wasm32"))]
#[path = "../src/refresh.rs"]
mod refresh;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Mutex;

#[test]
fn refresh_retains_exclusive_ownership_across_io_and_error_returns() {
    shuttle::check_dfs(
        || {
            shuttle::future::block_on(async {
                let lock = Arc::new(Mutex::new(()));
                let rotated = Arc::new(AtomicUsize::new(0));
                let active = Arc::new(AtomicUsize::new(0));
                let mut tasks = Vec::new();
                for fail in [false, true] {
                    let (lock, rotated, active) = (lock.clone(), rotated.clone(), active.clone());
                    tasks.push(shuttle::future::spawn(async move {
                        refresh::serialized(&lock, async {
                            assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                            let before = rotated.load(Ordering::SeqCst);
                            shuttle::future::yield_now().await;
                            assert_eq!(rotated.load(Ordering::SeqCst), before);
                            if !fail && before == 0 {
                                rotated.store(1, Ordering::SeqCst);
                            }
                            assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                            if fail { Err(()) } else { Ok(()) }
                        })
                        .await
                    }));
                }
                for task in tasks {
                    let _result = task.await.unwrap();
                }
                assert_eq!(rotated.load(Ordering::SeqCst), 1);
                assert_eq!(active.load(Ordering::SeqCst), 0);
                assert!(lock.try_lock().is_ok());
            });
        },
        None,
    );
}

#[test]
fn cancelling_the_holder_releases_the_guard_and_wakes_the_waiter() {
    shuttle::check_dfs(
        || {
            shuttle::future::block_on(async {
                let lock = Arc::new(Mutex::new(()));
                let (entered, entry) = futures::channel::oneshot::channel();
                let (abort, registration) = futures::future::AbortHandle::new_pair();
                let holder_lock = lock.clone();
                let holder = shuttle::future::spawn(futures::future::Abortable::new(
                    async move {
                        refresh::serialized(&holder_lock, async move {
                            entered.send(()).unwrap();
                            futures::future::pending::<()>().await;
                        })
                        .await;
                    },
                    registration,
                ));
                let waiter_lock = lock.clone();
                let waiter = shuttle::future::spawn(async move {
                    refresh::serialized(&waiter_lock, async {}).await;
                });
                entry.await.unwrap();
                abort.abort();
                assert!(holder.await.unwrap().is_err());
                waiter.await.unwrap();
                assert!(lock.try_lock().is_ok());
            });
        },
        None,
    );
}

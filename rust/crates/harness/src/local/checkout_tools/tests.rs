use super::*;
use futures::executor::block_on;
use loom::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

fn checked_model(scenario: fn(bool), diagnostic: &str) {
    loom::model(move || scenario(false));
    let failure = std::panic::catch_unwind(|| loom::model(move || scenario(true)))
        .expect_err("negative control did not expose the broken boundary");
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains(diagnostic),
        "unexpected negative control: {message}"
    );
}

// Both variants call the production serialization function and its cfg(loom)
// mutex shim. The negative control releases ownership before publication.
fn publication(broken: bool) {
    let state = StateArc::new(AsyncMutex::new((0, Some(0))));
    let commands: Vec<_> = (0..2)
        .map(|_| {
            let state = StateArc::clone(&state);
            thread::spawn(move || {
                block_on(serialized(&state, async |mut cursor| {
                    let generation = cursor.0;
                    assert_eq!(cursor.1.take(), Some(generation), "cursor torn");
                    if broken {
                        drop(cursor);
                        thread::yield_now();
                        let mut cursor = lock_owned(StateArc::clone(&state)).await;
                        cursor.0 = generation + 1;
                        cursor.1 = Some(cursor.0);
                    } else {
                        thread::yield_now();
                        cursor.0 += 1;
                        cursor.1 = Some(cursor.0);
                    }
                }))
            })
        })
        .collect();
    for command in commands {
        command.join().unwrap();
    }
    assert_eq!(*block_on(lock_owned(state)), (2, Some(2)), "cursor torn");
}

#[test]
fn commands_publish_generation_and_cursor_without_interleaving() {
    checked_model(publication, "cursor torn");
}

fn cancellation(broken: bool) {
    let state = StateArc::new(AsyncMutex::new(Some(0)));
    let mut command = Box::pin(serialized(&state, async |mut cursor| {
        if !broken {
            cursor.take();
        }
        std::future::pending::<()>().await;
    }));
    assert_eq!(
        command
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    );
    drop(command);
    assert!(
        block_on(lock_owned(state)).is_none(),
        "cancelled capture retained its cursor"
    );
}

#[test]
fn cancellation_releases_the_guard_and_invalidates_the_cursor() {
    checked_model(cancellation, "cancelled capture retained its cursor");
}

fn offloaded_cleanup(broken: bool) {
    let state = StateArc::new(AsyncMutex::new(()));
    let cleaned = StateArc::new(AtomicBool::new(false));
    let completed = StateArc::clone(&cleaned);
    let mut backend = None;
    let mut command = Box::pin(serialized(&state, async |guard| {
        if broken {
            drop(guard);
            backend = Some(thread::spawn(move || {
                thread::yield_now();
                completed.store(true, Ordering::SeqCst);
            }));
        } else {
            backend = Some(thread::spawn(move || {
                guarded(guard, |_| {
                    thread::yield_now();
                    completed.store(true, Ordering::SeqCst);
                });
            }));
        }
        std::future::pending::<()>().await;
    }));
    assert_eq!(
        command
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    );
    drop(command); // The awaiting task is cancelled after submitting the job.
    block_on(serialized(&state, async |_| {
        assert!(
            cleaned.load(Ordering::SeqCst),
            "cleanup escaped checkout ownership"
        );
    }));
    backend.unwrap().join().unwrap();
}

#[test]
fn cancelled_waiter_keeps_a_submitted_jobs_guard_until_cleanup() {
    checked_model(offloaded_cleanup, "cleanup escaped checkout ownership");
}

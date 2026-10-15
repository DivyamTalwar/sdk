use super::*;
use loom::{
    future::block_on,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[derive(Clone, Copy)]
enum Fault {
    None,
    LostWake,
    RetainedFence,
    WakeUnderLock,
}

fn shutdown_model(fault: Fault) {
    loom::model(move || {
        let result = shutdown(fault);
        assert!(result.is_ok(), "lifecycle model failed: {result:?}");
    });
}

fn shutdown(fault: Fault) -> Result<()> {
    let lifetime = Arc::new(Lifetime::new(
        tempfile::tempfile().map_err(|error| Error::Storage(error.to_string()))?,
        (),
    ));
    let serving = lifetime.clone();
    let owner = thread::spawn(move || {
        if serving.begin().is_ok() {
            // Cancellation and shutdown can race before the drain runs.
            drop(serving.stop_on_drop());
            block_on(serving.stopping());
            let retained = if matches!(fault, Fault::RetainedFence) {
                serving
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .instance
                    .take()
            } else {
                None
            };
            serving.release();
            if let Some(instance) = retained {
                serving
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .instance = Some(instance);
            }
        }
    });
    if lifetime.request_stop().0 {
        lifetime.release();
    }
    block_on(lifetime.stopped())?;
    owner
        .join()
        .map_err(|_| Error::Storage("lifecycle owner panicked".into()))?;
    let state = lifetime
        .state
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        state.phase == Phase::Stopped,
        "shutdown did not reach terminal state"
    );
    assert!(
        state.instance.is_none(),
        "terminal lifetime retains its instance fence"
    );
    drop(state);
    assert!(lifetime.begin().is_err(), "shutdown admitted a late start");
    Ok(())
}

#[test]
fn shutdown_cancellation_and_start_explore_all_mutex_orders() {
    shutdown_model(Fault::None);
}

#[test]
#[should_panic(expected = "shutdown transition lost a registered wakeup")]
fn negative_control_detects_a_discarded_shutdown_wakeup() {
    notification_model(Fault::LostWake);
}

#[test]
#[should_panic(expected = "terminal lifetime retains its instance fence")]
fn negative_control_detects_a_torn_terminal_state() {
    shutdown_model(Fault::RetainedFence);
}

struct Reenter {
    lifetime: Arc<Lifetime<()>>,
    notified: Arc<AtomicBool>,
}
impl std::task::Wake for Reenter {
    fn wake(self: std::sync::Arc<Self>) {
        assert!(
            self.lifetime.state.try_lock().is_ok(),
            "woken under the state lock"
        );
        self.notified.store(true, Ordering::SeqCst);
    }
}

fn notification_model(fault: Fault) {
    loom::model(move || {
        let result = (|| {
            let lifetime = Arc::new(Lifetime::new(
                tempfile::tempfile().map_err(|error| Error::Storage(error.to_string()))?,
                (),
            ));
            let notified = Arc::new(AtomicBool::new(false));
            let waker = Waker::from(std::sync::Arc::new(Reenter {
                lifetime: lifetime.clone(),
                notified: notified.clone(),
            }));
            let mut cx = Context::from_waker(&waker);
            let mut stopped = Box::pin(lifetime.stopped());
            assert!(stopped.as_mut().poll(&mut cx).is_pending());
            if matches!(fault, Fault::LostWake) {
                lifetime
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .waiters
                    .clear();
            }
            if matches!(fault, Fault::WakeUnderLock) {
                let mut state = lifetime
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                for waiter in std::mem::take(&mut state.waiters) {
                    waiter.wake();
                }
            } else {
                lifetime.release();
            }
            assert!(
                notified.load(Ordering::SeqCst),
                "shutdown transition lost a registered wakeup"
            );
            assert!(stopped.as_mut().poll(&mut cx).is_ready());
            Ok::<(), Error>(())
        })();
        assert!(result.is_ok(), "lock order model failed: {result:?}");
    });
}

#[test]
fn notification_releases_the_state_lock_before_waking() {
    notification_model(Fault::None);
}

#[test]
#[should_panic(expected = "woken under the state lock")]
fn negative_control_detects_waking_under_the_state_lock() {
    notification_model(Fault::WakeUnderLock);
}

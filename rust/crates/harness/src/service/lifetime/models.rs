use super::*;
use loom::{future::block_on, sync::Arc, thread};

#[derive(Clone, Copy)]
enum Fault {
    None,
    LostWake,
    RetainedFence,
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
            if matches!(fault, Fault::LostWake) {
                serving
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .waiters
                    .clear();
            }
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
#[should_panic(expected = "deadlock")]
fn negative_control_detects_a_discarded_shutdown_wakeup() {
    shutdown_model(Fault::LostWake);
}

#[test]
#[should_panic(expected = "terminal lifetime retains its instance fence")]
fn negative_control_detects_a_torn_terminal_state() {
    shutdown_model(Fault::RetainedFence);
}

struct Reenter(Arc<Lifetime<()>>);
impl std::task::Wake for Reenter {
    fn wake(self: std::sync::Arc<Self>) {
        drop(self.0.state.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

fn lock_order_model(broken: bool) {
    loom::model(move || {
        let result = (|| {
            let lifetime = Arc::new(Lifetime::new(
                tempfile::tempfile().map_err(|error| Error::Storage(error.to_string()))?,
                (),
            ));
            let waker = Waker::from(std::sync::Arc::new(Reenter(lifetime.clone())));
            let cx = Context::from_waker(&waker);
            assert!(lifetime.poll_phase(Phase::Stopped, &cx).is_pending());
            if broken {
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
            Ok::<(), Error>(())
        })();
        assert!(result.is_ok(), "lock order model failed: {result:?}");
    });
}

#[test]
fn notification_releases_the_state_lock_before_waking() {
    lock_order_model(false);
}

#[test]
#[should_panic(expected = "lock")]
fn negative_control_detects_waking_under_the_state_lock() {
    lock_order_model(true);
}

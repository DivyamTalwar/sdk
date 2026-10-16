//! Single-executor schedules. No thread, timer, detached job or global state.
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

#[derive(Default)]
struct State {
    recording: bool,
    cancelled: bool,
    active: usize,
    trace: Vec<&'static str>,
    points: Vec<&'static str>,
    publication: String,
    cancelled_at: Option<String>,
}

#[derive(Default)]
pub(super) struct Schedule(Mutex<State>);

impl Schedule {
    // Test-only state is accessed on one executor thread. No guard crosses an
    // await, and this is the sole lock; it never calls back into a provider.
    pub fn start(&self) {
        let mut state = self.0.lock().unwrap();
        assert_eq!(state.active, 0);
        *state = State {
            recording: true,
            ..State::default()
        };
    }

    pub async fn point(&self, label: &'static str) {
        let recording = {
            let mut state = self.0.lock().unwrap();
            if state.recording {
                state.points.push(label);
                state.trace.push(label);
            }
            state.recording
        };
        if recording {
            let mut yielded = false;
            futures::future::poll_fn(|cx| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
        }
    }

    // Reference-provider reads are immediately ready and change no durable
    // state. Trace them without inventing an extra suspension or I/O job.
    pub fn observe(&self, label: &'static str) {
        let mut state = self.0.lock().unwrap();
        if state.recording {
            state.trace.push(label);
        }
    }

    pub fn describe(&self, publication: impl FnOnce() -> String) {
        let mut state = self.0.lock().unwrap();
        if state.recording {
            state.publication = publication();
        }
    }

    pub fn failure(&self) -> String {
        let state = self.0.lock().unwrap();
        state
            .cancelled_at
            .clone()
            .unwrap_or_else(|| state.publication.clone())
    }

    pub fn dispatch(self: &Arc<Self>, label: &'static str) -> Active {
        let mut state = self.0.lock().unwrap();
        assert!(!state.cancelled, "dispatch after cancellation: {label}");
        state.active += 1;
        if state.recording {
            state.trace.push(label);
        }
        Active(self.clone())
    }

    pub fn cancel(&self) {
        let mut state = self.0.lock().unwrap();
        state.cancelled_at = Some(format!("{:?}: {}", state.points.last(), state.publication));
        state.cancelled = true;
        state.trace.push("CANCEL");
    }

    pub fn drained(&self) -> Vec<&'static str> {
        let state = self.0.lock().unwrap();
        assert_eq!(state.active, 0, "recovery raced outstanding work");
        assert_eq!(state.trace.last(), Some(&"CANCEL"));
        state.trace.clone()
    }

    pub fn recover(&self) {
        let mut state = self.0.lock().unwrap();
        assert_eq!(state.active, 0);
        state.recording = false;
        state.cancelled = false;
    }

    pub fn points(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().points.clone()
    }
}

pub(super) struct Active(Arc<Schedule>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().active -= 1;
    }
}

pub(super) enum Exit<T> {
    Cancelled,
    Completed(T),
}

#[derive(Default)]
struct Wake(AtomicBool);
impl futures::task::ArcWake for Wake {
    fn wake_by_ref(this: &Arc<Self>) {
        this.0.store(true, Ordering::SeqCst);
    }
}

// One point is exactly one Pending poll. Uncontrolled Pending is a test error,
// not permission to silently change or truncate a replay schedule.
pub(super) fn drive<F: Future>(
    future: F,
    cut: Option<usize>,
    schedule: &Schedule,
) -> (Exit<F::Output>, usize) {
    let mut future = Box::pin(future);
    let wake = Arc::new(Wake::default());
    let waker = futures::task::waker(wake.clone());
    let mut cx = Context::from_waker(&waker);
    let mut pending = 0;
    loop {
        wake.0.store(false, Ordering::SeqCst);
        match Pin::as_mut(&mut future).poll(&mut cx) {
            Poll::Ready(output) => return (Exit::Completed(output), pending),
            Poll::Pending => {
                assert!(
                    wake.0.swap(false, Ordering::SeqCst),
                    "executor has no ready work: uncontrolled I/O at {:?}",
                    schedule.points()
                );
                pending += 1;
                if cut == Some(pending) {
                    // Mark cancellation before dropping the suspended future,
                    // including its provider calls and all their destructors.
                    schedule.cancel();
                    return (Exit::Cancelled, pending);
                }
            }
        }
    }
}

pub(super) fn check_cut<T>(exit: &Exit<T>, cut: usize, hit: usize) -> Result<(), &'static str> {
    if matches!(exit, Exit::Cancelled) && cut == hit {
        Ok(())
    } else {
        Err("cancellation point was skipped")
    }
}

#[test]
fn dispatch_after_cancellation_is_rejected() {
    let schedule = Arc::new(Schedule::default());
    schedule.start();
    schedule.cancel();
    assert!(std::panic::catch_unwind(|| schedule.dispatch("broken detached dispatch")).is_err());
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn every_skipped_point_is_detected(length in 1usize..16, omitted in 0usize..16) {
        let omitted = omitted % length;
        let schedule = Schedule::default();
        schedule.start();
        let (exit, hit) = drive(async {
            for index in 0..length {
                if index != omitted { schedule.point("point").await; }
            }
        }, Some(length), &schedule);
        proptest::prop_assert!(check_cut(&exit, length, hit).is_err());
    }
}

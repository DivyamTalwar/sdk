//! Explicit, scoped barriers at real owned-task and native-job boundaries.
use std::{
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, mpsc, oneshot};

tokio::task_local! { static CURRENT: Context; }

/// A submitted operation paused before execution or acknowledgement.
pub struct Boundary {
    /// Stable phase, independent of executor poll counts.
    pub name: &'static str,
    /// Owning operation, including its stream's tracing span.
    pub detail: String,
    release: Option<oneshot::Sender<()>>,
}

impl Boundary {
    /// Allows the original owned operation to continue.
    pub fn release(mut self) {
        self.release.take().unwrap().send(()).ok();
    }
}

#[derive(Default)]
struct State {
    cancelled: bool,
    trace: Vec<String>,
}

struct Inner {
    events: mpsc::UnboundedSender<Boundary>,
    active: AtomicUsize,
    idle: Notify,
    // Sole lock. Never held across an await, user callback, send, or wake.
    state: Mutex<State>,
}

/// One test execution; no process-global controller or background driver.
#[derive(Clone)]
pub struct Schedule(Arc<Inner>);

/// An original submission's cancellation-independent completion context.
#[derive(Clone)]
pub(crate) struct Context {
    schedule: Schedule,
    owned: bool,
    detail: Arc<Mutex<String>>,
}

pub(crate) struct Active(pub(crate) Context);
impl Drop for Active {
    fn drop(&mut self) {
        if self.0.schedule.0.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.schedule.0.idle.notify_one();
        }
    }
}

impl Schedule {
    /// Creates the push receiver used by a deterministic test driver.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Boundary>) {
        let (events, receiver) = mpsc::unbounded_channel();
        (
            Self(Arc::new(Inner {
                events,
                active: AtomicUsize::new(0),
                idle: Notify::new(),
                state: Mutex::new(State::default()),
            })),
            receiver,
        )
    }

    /// Instruments only this explicitly scoped future and its owned descendants.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        CURRENT
            .scope(
                Context {
                    schedule: self.clone(),
                    owned: false,
                    detail: Arc::new(Mutex::new(String::new())),
                },
                future,
            )
            .await
    }

    /// Marks cancellation before the caller's future is dropped.
    pub fn cancel(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.cancelled = true;
        state.trace.push("CANCEL".into());
    }

    /// Waits for accepted operations to reach their physical terminal.
    pub async fn drained(&self) {
        loop {
            let notified = self.0.idle.notified();
            if self.0.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    /// Original submissions and their completion trace, including cancellation.
    pub fn trace(&self) -> Vec<String> {
        self.0.state.lock().unwrap().trace.clone()
    }
}

impl Context {
    pub(crate) fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }

    pub(crate) fn accept(&self, kind: &'static str) -> Active {
        {
            let mut state = self.schedule.0.state.lock().unwrap();
            assert!(
                !state.cancelled || self.owned,
                "new dispatch after cancellation: {kind}"
            );
            state.trace.push(format!(
                "{kind}:{}",
                if self.owned { "completion" } else { "dispatch" }
            ));
        }
        self.schedule.0.active.fetch_add(1, Ordering::Release);
        Active(Self {
            schedule: self.schedule.clone(),
            owned: true,
            detail: Arc::new(Mutex::new(self.detail.lock().unwrap().clone())),
        })
    }

    async fn pause(&self, name: &'static str) {
        let (release, waiting) = oneshot::channel();
        self.schedule
            .0
            .events
            .send(Boundary {
                name,
                detail: self.detail.lock().unwrap().clone(),
                release: Some(release),
            })
            .unwrap();
        waiting
            .await
            .expect("driver dropped an accepted operation's barrier");
    }

    pub(crate) fn blocking(&self, name: &'static str) {
        let (release, waiting) = oneshot::channel();
        self.schedule
            .0
            .events
            .send(Boundary {
                name,
                detail: self.detail.lock().unwrap().clone(),
                release: Some(release),
            })
            .unwrap();
        waiting
            .blocking_recv()
            .expect("driver dropped an accepted native I/O barrier");
    }
}

/// Pauses a real provider read before dispatch or after completion.
pub async fn boundary(name: &'static str) {
    if let Some(context) = Context::current() {
        context.pause(name).await;
    }
}

/// Names the original storage operation without exposing its payload.
pub fn describe(detail: impl FnOnce() -> String) {
    if let Some(context) = Context::current() {
        *context.detail.lock().unwrap() = detail();
    }
}

/// Runs the same Tokio owned task with deterministic submission/terminal gates.
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let Some(context) = Context::current() else {
        return tokio::spawn(future);
    };
    let active = context.accept("task");
    let owned = active.0.clone();
    tokio::spawn(CURRENT.scope(owned.clone(), async move {
        let _active = active;
        owned.pause("task:submitted").await;
        let result = future.await;
        owned.pause("task:completed").await;
        result
    }))
}

/// Executes the real blocking closure, retaining ownership after caller drop.
pub fn spawn_blocking<F, T>(work: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let Some(context) = Context::current() else {
        return tokio::task::spawn_blocking(work);
    };
    let active = context.accept("blocking");
    tokio::task::spawn_blocking(move || {
        let _active = active;
        context.blocking("blocking:submitted");
        let result = work();
        context.blocking("blocking:completed");
        result
    })
}

//! Push barriers around the real native submissions; no timer or poll-index cuts.
use acyclic_native_runtime::testing::{Boundary, Schedule as IoSchedule};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Default)]
struct State {
    cancelled: bool,
    active: usize,
    trace: Vec<String>,
    points: Vec<&'static str>,
    publication: String,
    cancelled_at: Option<String>,
}

pub(super) struct Schedule {
    state: Mutex<State>,
    io: IoSchedule,
}

impl Schedule {
    pub fn new() -> (Arc<Self>, UnboundedReceiver<Boundary>) {
        let (io, events) = IoSchedule::new();
        (
            Arc::new(Self {
                state: Mutex::default(),
                io,
            }),
            events,
        )
    }
    // Sole test-state lock; no callback, send, or await while held.
    pub fn start(&self) {
        *self.state.lock().unwrap() = State::default();
    }
    pub async fn point(&self, name: &'static str) {
        acyclic_native_runtime::testing::boundary(name).await;
    }
    pub fn describe(&self, publication: impl FnOnce() -> String) {
        let publication = publication();
        self.state.lock().unwrap().publication = publication.clone();
        acyclic_native_runtime::testing::describe(|| publication);
    }
    pub fn failure(&self) -> String {
        let state = self.state.lock().unwrap();
        state
            .cancelled_at
            .clone()
            .unwrap_or_else(|| state.publication.clone())
    }
    pub fn dispatch(self: &Arc<Self>, label: &'static str) -> Active {
        let mut state = self.state.lock().unwrap();
        assert!(
            !state.cancelled,
            "external dispatch after cancellation: {label}"
        );
        state.active += 1;
        state.trace.push(label.into());
        Active(self.clone())
    }
    pub fn drained(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        assert_eq!(state.active, 0, "external effects remain active");
        let trace = self.io.trace();
        let cancel = trace.iter().position(|record| record == "CANCEL").unwrap();
        assert!(
            trace[cancel + 1..]
                .iter()
                .all(|record| record.ends_with(":completion")),
            "new dispatch after cancellation: {trace:?}"
        );
        trace
    }
    pub fn recover(&self) {
        self.state.lock().unwrap().cancelled = false;
    }
    pub fn points(&self) -> Vec<&'static str> {
        self.state.lock().unwrap().points.clone()
    }
}

pub(super) struct Active(Arc<Schedule>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active -= 1;
    }
}

pub(super) enum Exit<T> {
    Cancelled,
    Completed(T),
}

pub(super) async fn drive<F: Future>(
    future: F,
    cut: Option<usize>,
    schedule: &Schedule,
    events: &mut UnboundedReceiver<Boundary>,
) -> (Exit<F::Output>, usize) {
    let mut future = Box::pin(schedule.io.scope(future));
    let mut hit = 0;
    loop {
        tokio::select! {
            output = &mut future => return (Exit::Completed(output), hit),
            event = events.recv() => {
                let event = event.expect("provider lost its push driver");
                hit += 1;
                {
                    let mut state = schedule.state.lock().unwrap();
                    state.points.push(event.name);
                    state.publication = event.detail.clone();
                }
                if cut == Some(hit) {
                    {
                        let mut state = schedule.state.lock().unwrap();
                        state.cancelled = true;
                        state.cancelled_at = Some(format!("{}: {}", event.name, event.detail));
                    }
                    schedule.io.cancel();
                    // A JoinHandle's drop detaches its already submitted work.
                    // Cancel the caller first, then release and drain those owners.
                    drop(future);
                    event.release();
                    loop {
                        tokio::select! {
                            _ = schedule.io.drained() => break,
                            event = events.recv() => event.unwrap().release(),
                        }
                    }
                    return (Exit::Cancelled, hit);
                }
                event.release();
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

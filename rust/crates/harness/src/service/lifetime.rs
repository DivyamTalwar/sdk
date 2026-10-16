use super::sync::Mutex;
use crate::{Error, Result};
use std::{
    fs::File,
    future::poll_fn,
    sync::PoisonError,
    task::{Context, Poll, Waker},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Idle,
    Running,
    Stopping,
    Stopped,
}

/// Owns the instance fence until the listener and all connections are drained.
pub(super) struct Lifetime<L> {
    // Lock order: state only. No guard crosses an await or nests with the
    // listener/registry. Wakers run after unlocking, including reentrant ones.
    state: Mutex<State<L>>,
}

struct State<L> {
    phase: Phase,
    instance: Option<File>,
    listener: Option<L>,
    waiters: Vec<Waker>,
}

#[cfg(all(test, acyclic_harness_loom))]
#[path = "lifetime/models.rs"]
mod models;

/// Cancelling the caller stops the independently owned connection drain.
pub(super) struct StopOnDrop<'a, L>(&'a Lifetime<L>);

impl<L> Drop for StopOnDrop<'_, L> {
    fn drop(&mut self) {
        self.0.request_stop();
    }
}

impl<L> Lifetime<L> {
    pub fn stop_on_drop(&self) -> StopOnDrop<'_, L> {
        StopOnDrop(self)
    }

    pub fn new(instance: File, listener: L) -> Self {
        Self {
            state: Mutex::new(State {
                phase: Phase::Idle,
                instance: Some(instance),
                listener: Some(listener),
                waiters: Vec::new(),
            }),
        }
    }

    pub fn begin(&self) -> Result<L> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.phase != Phase::Idle {
            return Err(Error::Conflict("service already started or stopped".into()));
        }
        state.phase = Phase::Running;
        state
            .listener
            .take()
            .ok_or_else(|| Error::Conflict("service listener is absent".into()))
    }

    pub fn request_stop(&self) -> (bool, Option<L>) {
        let (unstarted, listener, waiters) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(state.phase, Phase::Stopping | Phase::Stopped) {
                return (false, None);
            }
            let unstarted = state.phase == Phase::Idle;
            state.phase = Phase::Stopping;
            (
                unstarted,
                state.listener.take(),
                std::mem::take(&mut state.waiters),
            )
        };
        for waiter in waiters {
            waiter.wake();
        }
        (unstarted, listener)
    }

    pub fn release(&self) {
        let waiters = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            drop(state.instance.take());
            state.phase = Phase::Stopped;
            std::mem::take(&mut state.waiters)
        };
        for waiter in waiters {
            waiter.wake();
        }
    }

    fn poll_phase(&self, target: Phase, cx: &Context<'_>) -> Poll<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.phase == target || state.phase == Phase::Stopped {
            return Poll::Ready(());
        }
        // Checking and registering share the transition lock: a transition
        // either sees this waiter, or this poll sees the completed transition.
        if !state
            .waiters
            .iter()
            .any(|waiter| waiter.will_wake(cx.waker()))
        {
            state.waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }

    pub async fn stopping(&self) {
        poll_fn(|cx| self.poll_phase(Phase::Stopping, cx)).await
    }

    pub async fn stopped(&self) -> Result<()> {
        poll_fn(|cx| self.poll_phase(Phase::Stopped, cx)).await;
        Ok(())
    }
}

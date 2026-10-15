use super::storage;
use crate::{Error, Result};
use std::fs::File;
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Idle,
    Running,
    Stopping,
    Stopped,
}

/// Owns the instance fence until the listener and all connections are drained.
pub(super) struct Lifetime {
    // One watch lock owns both state and fence. No lifetime guard crosses an
    // await or is nested with the listener or workspace registry locks.
    state: watch::Sender<State>,
}

pub(super) struct State {
    pub phase: Phase,
    instance: Option<File>,
}

#[cfg(test)]
mod models;

/// Cancelling the caller stops the independently owned connection drain.
pub(super) struct StopOnDrop<'a>(&'a Lifetime);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.request_stop();
    }
}

impl Lifetime {
    pub fn stop_on_drop(&self) -> StopOnDrop<'_> {
        StopOnDrop(self)
    }

    pub fn new(instance: File) -> Self {
        Self {
            state: watch::channel(State {
                phase: Phase::Idle,
                instance: Some(instance),
            })
            .0,
        }
    }

    pub fn begin(&self) -> Result<watch::Receiver<State>> {
        if !self.state.send_if_modified(|state| {
            if state.phase != Phase::Idle {
                return false;
            }
            state.phase = Phase::Running;
            true
        }) {
            return Err(Error::Conflict("service already started or stopped".into()));
        }
        Ok(self.state.subscribe())
    }

    pub fn request_stop(&self) -> bool {
        let mut unstarted = false;
        self.state.send_if_modified(|state| {
            unstarted = state.phase == Phase::Idle;
            if matches!(state.phase, Phase::Stopping | Phase::Stopped) {
                return false;
            }
            state.phase = Phase::Stopping;
            true
        });
        unstarted
    }

    pub fn release(&self) {
        self.state.send_modify(|state| {
            drop(state.instance.take());
            state.phase = Phase::Stopped;
        });
    }

    pub async fn stopped(&self) -> Result<()> {
        self.state
            .subscribe()
            .wait_for(|state| state.phase == Phase::Stopped)
            .await
            .map_err(storage)?;
        Ok(())
    }
}

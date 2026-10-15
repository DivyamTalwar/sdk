use super::storage;
use crate::{Error, Result};
use std::{fs::File, sync::Mutex};
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
    phase: watch::Sender<Phase>,
    instance: Mutex<Option<File>>,
}

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
            phase: watch::channel(Phase::Idle).0,
            instance: Mutex::new(Some(instance)),
        }
    }

    pub fn begin(&self) -> Result<watch::Receiver<Phase>> {
        if !self.phase.send_if_modified(|phase| {
            if *phase != Phase::Idle {
                return false;
            }
            *phase = Phase::Running;
            true
        }) {
            return Err(Error::Conflict("service already started or stopped".into()));
        }
        Ok(self.phase.subscribe())
    }

    pub fn request_stop(&self) -> bool {
        let mut unstarted = false;
        self.phase.send_if_modified(|phase| {
            unstarted = *phase == Phase::Idle;
            if matches!(*phase, Phase::Stopping | Phase::Stopped) {
                return false;
            }
            *phase = Phase::Stopping;
            true
        });
        unstarted
    }

    pub fn release(&self) -> Result<()> {
        drop(self.instance.lock().map_err(storage)?.take());
        self.phase.send_replace(Phase::Stopped);
        Ok(())
    }

    pub async fn stopped(&self) -> Result<()> {
        self.phase
            .subscribe()
            .wait_for(|phase| *phase == Phase::Stopped)
            .await
            .map_err(storage)?;
        Ok(())
    }
}

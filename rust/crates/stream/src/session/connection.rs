//! Conflated connection generations, independent of task record delivery.
use super::sync::{Arc, Mutex};
use crate::StreamError;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

/// Push observation of successful physical session handshakes.
#[derive(Clone)]
pub struct Connections(Arc<Mutex<State>>);
struct State {
    generation: u64,
    next_id: u64,
    waiters: BTreeMap<u64, Waker>,
}
impl Connections {
    pub(super) fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            generation: 0,
            next_id: 0,
            waiters: BTreeMap::new(),
        })))
    }
    pub(super) fn advance(&self) -> Result<(), StreamError> {
        self.publish(true)
    }
    fn publish(&self, wake: bool) -> Result<(), StreamError> {
        // Lock order: never nested with pool, cursor or signal locks.
        // Registration and generation observation are atomic. Wake after unlock.
        let waiters = {
            let mut state = self.0.lock().map_err(|_| StreamError::Unavailable)?;
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or(StreamError::Capacity)?;
            std::mem::take(&mut state.waiters)
        };
        if wake {
            for (_, waiter) in waiters {
                waiter.wake();
            }
        }
        Ok(())
    }
    /// Waits for a newer generation, including a recovery already observed.
    pub async fn changed(&self, after: u64) -> Result<u64, StreamError> {
        Change {
            connections: self,
            after,
            id: None,
        }
        .await
    }
}
struct Change<'a> {
    connections: &'a Connections,
    after: u64,
    id: Option<u64>,
}
impl Future for Change<'_> {
    type Output = Result<u64, StreamError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let connections = self.connections;
        let mut state = connections.0.lock().map_err(|_| StreamError::Unavailable)?;
        if state.generation > self.after {
            return Poll::Ready(Ok(state.generation));
        }
        let id = match self.id {
            Some(id) => id,
            None => {
                if state.waiters.len() == crate::MAX_ITEMS {
                    return Poll::Ready(Err(StreamError::Capacity));
                }
                let id = state.next_id;
                state.next_id = id.checked_add(1).ok_or(StreamError::Capacity)?;
                self.id = Some(id);
                id
            }
        };
        state.waiters.insert(id, cx.waker().clone());
        Poll::Pending
    }
}
impl Drop for Change<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id
            && let Ok(mut state) = self.connections.0.lock()
        {
            state.waiters.remove(&id);
        }
    }
}

#[cfg(acyclic_stream_loom)]
/// Explores handshake publication concurrent with a waiter parking or cancelling.
pub fn verify_connections(omit_wake: bool) {
    loom::model(move || {
        use futures::task::{ArcWake, waker};
        use loom::sync::atomic::{AtomicUsize, Ordering};
        struct Wake(AtomicUsize);
        impl ArcWake for Wake {
            fn wake_by_ref(value: &std::sync::Arc<Self>) {
                value.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let connections = Connections::new();
        let publishing = connections.clone();
        let wake = std::sync::Arc::new(Wake(AtomicUsize::new(0)));
        let writer = loom::thread::spawn(move || publishing.publish(!omit_wake).unwrap());
        let waker = waker(wake.clone());
        let mut cx = Context::from_waker(&waker);
        let mut change = Box::pin(connections.changed(0));
        let parked = change.as_mut().poll(&mut cx).is_pending();
        writer.join().unwrap();
        if parked {
            assert!(
                wake.0.load(Ordering::SeqCst) > 0,
                "connection publication lost its wakeup"
            );
        }
        assert!(matches!(change.as_mut().poll(&mut cx), Poll::Ready(Ok(1))));
        drop(change);
        let mut cancelled = Box::pin(connections.changed(1));
        assert!(cancelled.as_mut().poll(&mut cx).is_pending());
        drop(cancelled);
        assert!(connections.0.lock().unwrap().waiters.is_empty());
    });
}

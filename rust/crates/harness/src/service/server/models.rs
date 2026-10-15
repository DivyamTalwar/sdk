use super::*;
use crate::service::model;
use shuttle::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

struct PendingCall(Arc<AtomicBool>);

impl Future for PendingCall {
    type Output = Result<()>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

impl Drop for PendingCall {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn disconnect_exhaustively_cancels_pending_dispatch() {
    disconnect_model(false);
}

#[test]
#[should_panic(expected = "deadlock")]
fn negative_control_detects_a_call_that_ignores_disconnect() {
    disconnect_model(true);
}

fn disconnect_model(broken: bool) {
    model::check(move || async move {
        let (client, server) = tokio::io::duplex(64);
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let handler = shuttle::future::spawn(async move {
            let mut socket = frames::connection(server);
            let response = if broken {
                PendingCall(observed).await.map(Some)
            } else {
                connected(&mut socket, PendingCall(observed)).await
            }?;
            assert!(response.is_none());
            Ok(())
        });
        let disconnect = shuttle::future::spawn(async move {
            drop(client);
            Ok(())
        });
        model::join(handler).await?;
        model::join(disconnect).await?;
        assert!(dropped.load(Ordering::SeqCst));
        Ok(())
    });
}

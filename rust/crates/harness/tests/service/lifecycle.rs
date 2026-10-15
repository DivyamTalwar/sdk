use super::*;
use acyclic_stream::{BoxProviderFuture, BoxProviderStream};
use std::{future::pending, time::Duration};
use tokio::sync::Notify;

#[derive(Default)]
struct PendingHost {
    entered: Notify,
    dropped: Notify,
}

struct DropSignal<'a>(&'a Notify);

impl Drop for DropSignal<'_> {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

impl PendingHost {
    fn wait<T: Send>(&self) -> BoxProviderFuture<'_, Result<T>> {
        Box::pin(async {
            let _signal = DropSignal(&self.dropped);
            self.entered.notify_one();
            pending().await
        })
    }
}

impl HarnessWireApi for PendingHost {
    fn handshake<'a>(
        &'a self,
        _: wire::HandshakeRequest,
    ) -> BoxProviderFuture<'a, Result<wire::HandshakeResponse>> {
        self.wait()
    }

    fn submit<'a>(
        &'a self,
        _: wire::CommandEnvelope,
    ) -> BoxProviderFuture<'a, Result<wire::Admission>> {
        self.wait()
    }

    fn observe<'a>(
        &'a self,
        _: wire::ObserveRequest,
    ) -> BoxProviderFuture<'a, Result<wire::OperationStatus>> {
        self.wait()
    }

    fn cancel<'a>(
        &'a self,
        _: wire::CancelRequest,
    ) -> BoxProviderFuture<'a, Result<wire::CancelResponse>> {
        self.wait()
    }

    fn replay<'a>(
        &'a self,
        _: wire::ResumeRequest,
    ) -> BoxProviderFuture<'a, Result<BoxProviderStream<'static, Result<wire::Delivery>>>> {
        self.wait()
    }
}

async fn signalled(signal: &Notify) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(1), signal.notified())
        .await
        .map_err(storage)
}

#[tokio::test]
async fn cancelled_serve_drains_connections_and_releases_the_instance_lock() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let service = Arc::new(HarnessService::bind(endpoint.clone())?);
    let host = Arc::new(PendingHost::default());
    service.register("pending".into(), host.clone())?;
    let serving = tokio::spawn(service.clone().serve());
    let client = LocalHarnessClient::new(endpoint.clone(), "pending".into());
    let call = tokio::spawn(async move { client.handshake(handshake()).await });
    signalled(&host.entered).await?;

    serving.abort();
    assert!(serving.await.is_err_and(|error| error.is_cancelled()));
    signalled(&host.dropped).await?;
    // Keep the service alive: dropping its last Arc must not be needed for cleanup.
    tokio::time::timeout(Duration::from_secs(1), async {
        let (first, second) = tokio::join!(service.shutdown(), service.shutdown());
        first?;
        second
    })
    .await
    .map_err(storage)??;
    assert!(call.await.map_err(storage)?.is_err());
    let replacement = HarnessService::bind(endpoint)?;
    replacement.shutdown().await?;
    assert!(matches!(
        service.clone().serve().await,
        Err(Error::Conflict(_))
    ));
    Ok(())
}

#[tokio::test]
async fn disconnected_calls_drop_pending_host_io_without_stopping_the_service() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let service = Arc::new(HarnessService::bind(endpoint.clone())?);
    let host = Arc::new(PendingHost::default());
    service.register("pending".into(), host.clone())?;
    let serving = tokio::spawn(service.clone().serve());
    let client = LocalHarnessClient::new(endpoint, "pending".into());
    for method in ["handshake", "submit", "observe", "cancel", "replay"] {
        let client = client.clone();
        let call = tokio::spawn(async move {
            match method {
                "handshake" => client.handshake(handshake()).await.map(|_| ()),
                "submit" => client
                    .submit(wire::CommandEnvelope {
                        protocol: Some(current_protocol()),
                        ..Default::default()
                    })
                    .await
                    .map(|_| ()),
                "observe" => client
                    .observe(wire::ObserveRequest::default())
                    .await
                    .map(|_| ()),
                "cancel" => client
                    .cancel(wire::CancelRequest::default())
                    .await
                    .map(|_| ()),
                "replay" => {
                    let mut stream = client
                        .replay(wire::ResumeRequest {
                            protocol: Some(current_protocol()),
                            ..Default::default()
                        })
                        .await?;
                    stream.next().await.transpose().map(|_| ())
                }
                _ => unreachable!(),
            }
        });
        signalled(&host.entered).await?;
        call.abort();
        assert!(call.await.is_err_and(|error| error.is_cancelled()));
        // No host completion or global shutdown may be needed to drop this wait.
        signalled(&host.dropped).await?;
    }
    service.shutdown().await?;
    serving.await.map_err(storage)??;
    Ok(())
}

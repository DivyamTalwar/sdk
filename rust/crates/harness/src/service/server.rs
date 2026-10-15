use super::{
    ServiceEndpoint,
    frames::{self, Method},
};
use crate::{
    Error, Result, wire,
    wire_api::{self, HarnessWireApi},
};
use acyclic_stream::BoxProviderFuture;
use futures::StreamExt as _;
use prost::Message;
use std::sync::Arc;

#[cfg(test)]
mod models;

/// A single process hosting workspace wire APIs over UDS or Windows named pipes.
pub struct HarnessService {
    workspaces: super::registry::Registry<dyn HarnessWireApi>,
    lifetime: super::lifetime::Lifetime<super::listener::Listener>,
}

impl HarnessService {
    /// Acquires the lifetime process lock before binding or removing a stale endpoint.
    pub fn bind(endpoint: ServiceEndpoint) -> Result<Self> {
        let instance = endpoint.lock()?;
        let listener = super::listener::Listener::bind(endpoint)?;
        Ok(Self {
            lifetime: super::lifetime::Lifetime::new(instance, listener),
            workspaces: super::registry::Registry::new(),
        })
    }

    /// Registers an already opened workspace exactly once, before clients submit work.
    pub fn register(&self, workspace: String, api: Arc<dyn HarnessWireApi>) -> Result<()> {
        self.workspaces.insert(workspace, api)
    }

    /// Accepts clients until shutdown. Client disconnects only cancel their RPC.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let listener = self.lifetime.begin()?;
        let _stop = self.lifetime.stop_on_drop();
        // The drain must outlive cancellation of the public serving future.
        tokio::spawn(self.clone().serve_connections(listener))
            .await
            .map_err(super::storage)?
    }

    async fn serve_connections(
        self: Arc<Self>,
        mut listener: super::listener::Listener,
    ) -> Result<()> {
        let mut clients = tokio::task::JoinSet::new();
        let result = loop {
            tokio::select! {
                biased;
                _ = self.lifetime.stopping() => break Ok(()),
                _ = clients.join_next(), if !clients.is_empty() => {}
                socket = listener.accept() => {
                    let socket = match socket { Ok(socket) => socket, Err(error) => break Err(error) };
                    let service = self.clone();
                    clients.spawn(async move { service.handle(socket).await });
                }
            }
        };
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        drop(listener);
        self.lifetime.release();
        result
    }

    /// Stops accepting, drains connection tasks and releases the instance lock.
    pub async fn shutdown(&self) -> Result<()> {
        let (unstarted, listener) = self.lifetime.request_stop();
        drop(listener);
        if unstarted {
            self.lifetime.release();
        }
        self.lifetime.stopped().await
    }

    fn handle(&self, socket: frames::Connection) -> BoxProviderFuture<'_, Result<()>> {
        Box::pin(self.handle_connection(socket))
    }

    async fn handle_connection(&self, mut socket: frames::Connection) -> Result<()> {
        let request = frames::receive::<frames::Request>(&mut socket).await?;
        let result = self.dispatch(&mut socket, request).await;
        if let Err(error) = result {
            frames::send(&mut socket, &frames::error(&error)).await?;
        }
        Ok(())
    }

    fn dispatch<'a>(
        &'a self,
        socket: &'a mut frames::Connection,
        request: frames::Request,
    ) -> BoxProviderFuture<'a, Result<()>> {
        Box::pin(self.dispatch_call(socket, request))
    }

    async fn dispatch_call(
        &self,
        socket: &mut frames::Connection,
        request: frames::Request,
    ) -> Result<()> {
        let method = Method::try_from(request.method)
            .map_err(|_| Error::Invalid("unknown local RPC".into()))?;
        let api = self.workspaces.get(&request.workspace)?;
        match method {
            Method::Handshake => {
                let Some(response) =
                    connected(socket, api.handshake(decode(request.payload)?)).await?
                else {
                    return Ok(());
                };
                frames::send(socket, &frames::response(response)).await
            }
            Method::Submit => {
                let command: wire::CommandEnvelope = decode(request.payload)?;
                wire_api::validate_command_protocol(&command)?;
                let expected = wire::CommandEnvelope {
                    operation: command.operation.clone(),
                    ..Default::default()
                };
                let Some(response) =
                    connected(socket, HarnessWireApi::submit(api.as_ref(), command)).await?
                else {
                    return Ok(());
                };
                wire_api::validate_admission(&expected, &response)?;
                frames::send(socket, &frames::response(response)).await
            }
            Method::Observe => {
                let request: wire::ObserveRequest = decode(request.payload)?;
                let Some(response) =
                    connected(socket, wire_api::observe(api.as_ref(), request)).await?
                else {
                    return Ok(());
                };
                frames::send(socket, &frames::response(response)).await
            }
            Method::Cancel => {
                let request: wire::CancelRequest = decode(request.payload)?;
                let Some(response) =
                    connected(socket, wire_api::cancel(api.as_ref(), request)).await?
                else {
                    return Ok(());
                };
                frames::send(socket, &frames::response(response)).await
            }
            Method::Replay => {
                let request: wire::ResumeRequest = decode(request.payload)?;
                wire_api::validate_resume_protocol(&request)?;
                let Some(mut deliveries) = connected(socket, api.replay(request)).await? else {
                    return Ok(());
                };
                // Reading detects disconnect even when the producer has no new events.
                loop {
                    tokio::select! {
                        biased;
                        _ = socket.next() => return Ok(()),
                        item = deliveries.next() => match item {
                            Some(item) => frames::send(socket, &frames::response(item?)).await?,
                            None => break,
                        }
                    }
                }
                frames::send(
                    socket,
                    &frames::Response {
                        end: true,
                        ..Default::default()
                    },
                )
                .await
            }
        }
    }
}

async fn connected<T>(
    socket: &mut frames::Connection,
    call: impl Future<Output = Result<T>>,
) -> Result<Option<T>> {
    // One call owns the connection. EOF, errors or another request end its wait.
    tokio::select! {
        biased;
        _ = socket.next() => Ok(None),
        response = call => response.map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::super::storage;
    use super::*;

    #[tokio::test]
    async fn connection_and_dispatch_futures_fit_in_small_stack_frames() -> Result<()> {
        let directory = tempfile::tempdir().map_err(storage)?;
        let service = HarnessService::bind(ServiceEndpoint::in_directory(
            directory.path().join("service"),
        )?)?;
        let (socket, _peer) = tokio::io::duplex(64);
        let handler = service.handle_connection(frames::connection(socket));
        let handler_bytes = std::mem::size_of_val(&handler);
        drop(handler);
        let (socket, _peer) = tokio::io::duplex(64);
        let mut socket = frames::connection(socket);
        let dispatch = service.dispatch_call(&mut socket, frames::Request::default());
        let dispatch_bytes = std::mem::size_of_val(&dispatch);
        eprintln!("per-connection state: {handler_bytes}; per-call state: {dispatch_bytes} bytes");
        assert!(
            handler_bytes < 16 * 1024,
            "handler state: {handler_bytes} bytes"
        );
        assert!(
            dispatch_bytes < 16 * 1024,
            "dispatch state: {dispatch_bytes} bytes"
        );
        drop(dispatch);
        let service = Arc::new(service);
        let server = service.clone().serve();
        assert!(std::mem::size_of_val(&server) < 16 * 1024);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_before_serve_releases_the_fence_and_prevents_a_late_start() -> Result<()> {
        let directory = tempfile::tempdir().map_err(storage)?;
        let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
        let service = Arc::new(HarnessService::bind(endpoint.clone())?);
        let (first, second) = tokio::join!(service.shutdown(), service.shutdown());
        first?;
        second?;
        assert!(matches!(
            service.clone().serve().await,
            Err(Error::Conflict(_))
        ));
        let replacement = HarnessService::bind(endpoint)?;
        replacement.shutdown().await?;
        Ok(())
    }
}

fn decode<M: Message + Default>(payload: bytes::Bytes) -> Result<M> {
    M::decode(payload).map_err(|error| Error::Invalid(error.to_string()))
}

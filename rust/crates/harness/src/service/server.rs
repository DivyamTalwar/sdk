use super::{
    ServiceEndpoint,
    frames::{self, Method},
    storage,
};
use crate::{
    Error, Result, wire,
    wire_api::{self, HarnessWireApi},
};
use futures::StreamExt as _;
use prost::Message;
use std::{collections::BTreeMap, fs::File, sync::Arc};
use tokio::sync::RwLock;

/// A single process hosting workspace wire APIs over UDS or Windows named pipes.
pub struct HarnessService {
    endpoint: ServiceEndpoint,
    workspaces: RwLock<BTreeMap<String, Arc<dyn HarnessWireApi>>>,
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
    #[cfg(windows)]
    listener: tokio::sync::Mutex<tokio::net::windows::named_pipe::NamedPipeServer>,
    _instance: File,
}

impl HarnessService {
    /// Acquires the lifetime process lock before binding or removing a stale endpoint.
    pub fn bind(endpoint: ServiceEndpoint) -> Result<Self> {
        let instance = endpoint.lock()?;
        #[cfg(unix)]
        let listener = {
            let path = endpoint.directory.join("harness.sock");
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
            }
            tokio::net::UnixListener::bind(path).map_err(storage)?
        };
        #[cfg(windows)]
        let listener = tokio::sync::Mutex::new(pipe(&endpoint, true)?);
        Ok(Self {
            endpoint,
            _instance: instance,
            workspaces: RwLock::new(BTreeMap::new()),
            listener,
        })
    }

    /// Registers an already opened workspace exactly once, before clients submit work.
    pub async fn register(&self, workspace: String, api: Arc<dyn HarnessWireApi>) -> Result<()> {
        let mut workspaces = self.workspaces.write().await;
        if workspaces.contains_key(&workspace) {
            return Err(Error::Conflict("workspace already registered".into()));
        }
        workspaces.insert(workspace, api);
        Ok(())
    }

    /// Accepts clients until this future is dropped. Client disconnects only cancel their RPC.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                socket = self.accept() => {
                    let socket = socket?;
                    let service = self.clone();
                    clients.spawn(async move { service.handle(socket).await });
                }
                _ = clients.join_next(), if !clients.is_empty() => {}
            }
        }
    }

    async fn accept(&self) -> Result<frames::Connection> {
        #[cfg(unix)]
        {
            self.listener
                .accept()
                .await
                .map(|(socket, _)| frames::connection(socket))
                .map_err(storage)
        }
        #[cfg(windows)]
        {
            let mut listener = self.listener.lock().await;
            listener.connect().await.map_err(storage)?;
            let next = pipe(&self.endpoint, false)?;
            Ok(frames::connection(std::mem::replace(&mut *listener, next)))
        }
    }

    async fn handle(&self, mut socket: frames::Connection) -> Result<()> {
        let request = frames::receive::<frames::Request>(&mut socket).await?;
        let result = self.dispatch(&mut socket, request).await;
        if let Err(error) = result {
            frames::send(&mut socket, &frames::error(&error)).await?;
        }
        Ok(())
    }

    async fn dispatch(
        &self,
        socket: &mut frames::Connection,
        request: frames::Request,
    ) -> Result<()> {
        let api = self
            .workspaces
            .read()
            .await
            .get(&request.workspace)
            .cloned()
            .ok_or_else(|| Error::NotFound("service workspace".into()))?;
        let method = Method::try_from(request.method)
            .map_err(|_| Error::Invalid("unknown local RPC".into()))?;
        match method {
            Method::Handshake => {
                frames::send(
                    socket,
                    &frames::response(api.handshake(decode(request.payload)?).await?),
                )
                .await
            }
            Method::Submit => {
                let command: wire::CommandEnvelope = decode(request.payload)?;
                wire_api::validate_command_protocol(&command)?;
                let expected = wire::CommandEnvelope {
                    operation: command.operation.clone(),
                    ..Default::default()
                };
                let response = api.submit(command).await?;
                wire_api::validate_admission(&expected, &response)?;
                frames::send(socket, &frames::response(response)).await
            }
            Method::Observe => {
                let request: wire::ObserveRequest = decode(request.payload)?;
                let control = wire_api::validate_observe_request(&request)?;
                api.authorize_operation_control(&control).await?;
                let response = api.observe(request.clone()).await?;
                wire_api::validate_operation_status(
                    &request.operation_id,
                    request.owner.as_ref(),
                    &response,
                )?;
                frames::send(socket, &frames::response(response)).await
            }
            Method::Cancel => {
                let request: wire::CancelRequest = decode(request.payload)?;
                let control = wire_api::validate_cancel_request(&request)?;
                api.authorize_operation_control(&control.control).await?;
                let response = api.cancel(request.clone()).await?;
                wire_api::validate_cancel_response(&request, &response)?;
                frames::send(socket, &frames::response(response)).await
            }
            Method::Authorize => {
                let request: wire::ObserveRequest = decode(request.payload)?;
                let control = if request.scope.as_ref().is_some_and(|scope| {
                    scope
                        .capabilities
                        .iter()
                        .any(|cap| cap == "operation:observe")
                }) {
                    wire_api::validate_observe_request(&request)?
                } else {
                    wire_api::validate_cancel_request(&wire::CancelRequest {
                        operation_id: request.operation_id,
                        protocol: request.protocol,
                        owner: request.owner,
                        scope: request.scope,
                        recursive: false,
                        idempotency_key: "authorize".into(),
                    })?
                    .control
                };
                api.authorize_operation_control(&control).await?;
                frames::send(
                    socket,
                    &frames::response(wire::HandshakeResponse {
                        protocol: Some(wire_api::current_protocol()),
                        supported: None,
                    }),
                )
                .await
            }
            Method::Replay => {
                let request: wire::ResumeRequest = decode(request.payload)?;
                wire_api::validate_resume_protocol(&request)?;
                let mut deliveries = api.replay(request).await?;
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

fn decode<M: Message + Default>(payload: bytes::Bytes) -> Result<M> {
    M::decode(payload).map_err(|error| Error::Invalid(error.to_string()))
}

#[cfg(windows)]
fn pipe(
    endpoint: &ServiceEndpoint,
    first: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(&endpoint.pipe)
        .map_err(storage)
}

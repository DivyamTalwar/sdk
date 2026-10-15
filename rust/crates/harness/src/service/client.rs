use super::{
    ServiceEndpoint,
    frames::{self, Method},
};
use crate::{
    Result, wire,
    wire_api::{HarnessWireApi, OperationControlRequest},
};
use acyclic_stream::{BoxProviderFuture, BoxProviderStream};
use prost::Message;

/// A workspace-bound client of the per-user service. Dropping it leaves the service running.
#[derive(Clone, Debug)]
pub struct LocalHarnessClient {
    endpoint: ServiceEndpoint,
    workspace: String,
}

impl LocalHarnessClient {
    /// Binds one service workspace key; the server resolves it before routing requests.
    #[must_use]
    pub fn new(endpoint: ServiceEndpoint, workspace: String) -> Self {
        Self {
            endpoint,
            workspace,
        }
    }

    async fn request<M: Message + Default + std::fmt::Debug>(
        &self,
        method: Method,
        request: M,
    ) -> Result<frames::Connection> {
        let mut socket = self.endpoint.connect().await?;
        frames::send(
            &mut socket,
            &frames::Call {
                workspace: self.workspace.clone(),
                method: method as i32,
                payload: Some(request),
            },
        )
        .await?;
        Ok(socket)
    }

    async fn unary<M: Message + Default + std::fmt::Debug, R: Message + Default>(
        &self,
        method: Method,
        request: M,
    ) -> Result<R> {
        let mut socket = self.request(method, request).await?;
        let response: frames::Response = frames::receive(&mut socket).await?;
        if let Some(error) = response.error {
            return Err(frames::remote(error));
        }
        if response.end {
            return Err(crate::Error::Invalid("unexpected end of local RPC".into()));
        }
        R::decode(response.payload).map_err(|error| crate::Error::Invalid(error.to_string()))
    }
}

impl HarnessWireApi for LocalHarnessClient {
    fn handshake<'a>(
        &'a self,
        request: wire::HandshakeRequest,
    ) -> BoxProviderFuture<'a, Result<wire::HandshakeResponse>> {
        Box::pin(async move { self.unary(Method::Handshake, request).await })
    }
    fn submit<'a>(
        &'a self,
        request: wire::CommandEnvelope,
    ) -> BoxProviderFuture<'a, Result<wire::Admission>> {
        Box::pin(async move { self.unary(Method::Submit, request).await })
    }
    fn replay<'a>(
        &'a self,
        request: wire::ResumeRequest,
    ) -> BoxProviderFuture<'a, Result<BoxProviderStream<'static, Result<wire::Delivery>>>> {
        Box::pin(async move {
            let socket = self.request(Method::Replay, request).await?;
            let stream = futures::stream::try_unfold(socket, |mut socket| async move {
                let response: frames::Response = frames::receive(&mut socket).await?;
                if let Some(error) = response.error {
                    return Err(frames::remote(error));
                }
                if response.end {
                    return Ok(None);
                }
                let delivery = wire::Delivery::decode(response.payload)
                    .map_err(|error| crate::Error::Invalid(error.to_string()))?;
                Ok(Some((delivery, socket)))
            });
            Ok(Box::pin(stream) as BoxProviderStream<'static, Result<wire::Delivery>>)
        })
    }
    fn authorize_operation_control<'a>(
        &'a self,
        request: &'a OperationControlRequest,
    ) -> BoxProviderFuture<'a, Result<()>> {
        Box::pin(async move {
            self.unary::<_, wire::HandshakeResponse>(
                Method::Authorize,
                wire::ObserveRequest {
                    operation_id: request.operation_id.to_string(),
                    protocol: Some(crate::wire_api::current_protocol()),
                    owner: Some(crate::wire_codec::encode_authority(&request.owner)),
                    scope: Some(wire::Scope::from(&request.scope)),
                },
            )
            .await
            .map(|_| ())
        })
    }

    fn observe<'a>(
        &'a self,
        request: wire::ObserveRequest,
    ) -> BoxProviderFuture<'a, Result<wire::OperationStatus>> {
        Box::pin(async move { self.unary(Method::Observe, request).await })
    }
    fn cancel<'a>(
        &'a self,
        request: wire::CancelRequest,
    ) -> BoxProviderFuture<'a, Result<wire::CancelResponse>> {
        Box::pin(async move { self.unary(Method::Cancel, request).await })
    }
}

//! Explicit lifecycle authority for transport probes. Local task credentials
//! deliberately do not receive lifecycle administration to serve these tests.
use super::harness;
use acyclic_stream::{BoxProviderFuture, BoxProviderStream, MemoryStream};
use harness::{
    Capabilities, Error, Result,
    core::{AggregateKind, Authority, AuthorityIssuer, AuthorityVerifier, SchemaRegistry, Scope},
    store::StreamAggregate,
    wire,
    wire_api::{self, HarnessWireApi},
    wire_codec::{decode_command_envelope, encode_authority},
};
use std::sync::Arc;

pub(super) struct HistoryHost {
    provider: Arc<MemoryStream>,
    verifier: AuthorityVerifier,
    scope: Scope,
}

impl HistoryHost {
    pub(super) fn new() -> Result<Arc<Self>> {
        let issuer = AuthorityIssuer::new(
            "socket-test",
            [7; 32],
            Authority {
                kind: AggregateKind::Task,
                id: "socket-test".into(),
            },
        );
        Ok(Arc::new(Self {
            provider: Arc::new(MemoryStream::default()),
            verifier: issuer.verifier(&mut None)?,
            scope: issuer.issue(
                "test",
                &Capabilities::new(["lifecycle:manage"]),
                None,
                None,
                &mut None,
            )?,
        }))
    }
    pub(super) fn owner(&self) -> (&Authority, &Scope) {
        (self.verifier.audience(), &self.scope)
    }
    pub(super) fn start_cursor(&self) -> wire::ReplayCursor {
        wire::ReplayCursor {
            authority: Some(encode_authority(self.verifier.audience())),
            generation: self.verifier.audience().id.clone(),
            revision: 0,
        }
    }
}

impl HarnessWireApi for HistoryHost {
    fn handshake<'a>(
        &'a self,
        request: wire::HandshakeRequest,
    ) -> BoxProviderFuture<'a, Result<wire::HandshakeResponse>> {
        Box::pin(async move { wire_api::negotiate(&request, &wire::CapabilitySet::default()) })
    }
    fn submit<'a>(
        &'a self,
        request: wire::CommandEnvelope,
    ) -> BoxProviderFuture<'a, Result<wire::Admission>> {
        Box::pin(async move {
            let operation = request.operation.clone();
            let (authority, command) = decode_command_envelope(request)?;
            self.verifier.verify_audience(&authority, &mut None)?;
            let mut aggregate = StreamAggregate::open(
                &self.provider,
                (authority, self.verifier.clone(), SchemaRegistry::new()),
                None,
                harness::store::default_projection_read_limits(),
                harness::store::DEFAULT_TERMINAL_EFFECTS,
                None,
                &mut None,
            )
            .await?;
            let (state, error) = match aggregate.execute(command, None, &mut None).await {
                Ok(_) => (wire::AdmissionState::Accepted, None),
                Err(error) => (
                    wire::AdmissionState::Rejected,
                    Some(harness::encode_error(&error)),
                ),
            };
            Ok(wire::Admission {
                operation,
                state: state as i32,
                error,
            })
        })
    }
    fn replay<'a>(
        &'a self,
        request: wire::ResumeRequest,
    ) -> BoxProviderFuture<'a, Result<BoxProviderStream<'static, Result<wire::Delivery>>>> {
        wire_api::replay_history(
            &self.provider,
            &self.verifier,
            &self.verifier.audience().id,
            request,
        )
    }
    fn observe<'a>(
        &'a self,
        _: wire::ObserveRequest,
    ) -> BoxProviderFuture<'a, Result<wire::OperationStatus>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "history probe has no task scheduler".into(),
            ))
        })
    }
    fn cancel<'a>(
        &'a self,
        _: wire::CancelRequest,
    ) -> BoxProviderFuture<'a, Result<wire::CancelResponse>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "history probe has no task scheduler".into(),
            ))
        })
    }
}

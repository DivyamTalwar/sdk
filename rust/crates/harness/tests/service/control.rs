use super::*;
use acyclic_stream::{BoxProviderFuture, BoxProviderStream};
use harness::{
    Capabilities,
    core::{AggregateKind, Authority, AuthorityIssuer, AuthorityVerifier},
    wire_api::{self, OperationControlRequest},
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct ControlProbe {
    owner: Authority,
    issuer: AuthorityIssuer,
    verifier: AuthorityVerifier,
    calls: AtomicUsize,
    allocation: Mutex<Option<usize>>,
    wrong_owner: bool,
}

impl ControlProbe {
    fn new() -> Result<Self> {
        let owner = Authority {
            kind: AggregateKind::Task,
            id: "owner".into(),
        };
        let issuer = AuthorityIssuer::new("probe", [9; 32], owner.clone());
        Ok(Self {
            owner,
            verifier: issuer.verifier(&mut None)?,
            issuer,
            calls: AtomicUsize::new(0),
            allocation: Mutex::new(None),
            wrong_owner: false,
        })
    }

    fn request(&self) -> Result<wire::ObserveRequest> {
        let scope = self.issuer.issue(
            "probe-scope",
            &Capabilities::new(["operation:observe", "operation:cancel"]),
            None,
            None,
            &mut None,
        )?;
        Ok(wire::ObserveRequest {
            protocol: Some(current_protocol()),
            owner: Some(harness::wire_codec::encode_authority(&self.owner)),
            operation_id: OperationId::from_bytes([12; 16]).to_string(),
            scope: Some(wire::Scope::from(&scope)),
        })
    }

    fn authenticate(&self, control: &OperationControlRequest) -> Result<wire::OperationStatus> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(allocation) = self.allocation.lock().map_err(storage)?.take() {
            assert_eq!(
                control.scope.id().as_ptr() as usize,
                allocation,
                "signed scope was cloned"
            );
        }
        self.verifier.verify_audience(&control.owner, &mut None)?;
        self.verifier.verify(&control.scope, &mut None)?;
        let mut owner = harness::wire_codec::encode_authority(&control.owner);
        if self.wrong_owner {
            owner.id = "another-owner".into();
        }
        Ok(wire::OperationStatus {
            protocol: Some(current_protocol()),
            owner: Some(owner),
            operation: Some(wire::OperationIdentity {
                operation_id: control.operation_id.to_string(),
                idempotency_key: String::new(),
            }),
            state: wire::CompletionState::Running as i32,
            ..Default::default()
        })
    }
}

impl HarnessWireApi for ControlProbe {
    fn handshake<'a>(
        &'a self,
        _: wire::HandshakeRequest,
    ) -> BoxProviderFuture<'a, Result<wire::HandshakeResponse>> {
        Box::pin(async { Err(Error::Unsupported("unused".into())) })
    }
    fn submit<'a>(
        &'a self,
        _: wire::CommandEnvelope,
    ) -> BoxProviderFuture<'a, Result<wire::Admission>> {
        Box::pin(async { Err(Error::Unsupported("unused".into())) })
    }
    fn replay<'a>(
        &'a self,
        _: wire::ResumeRequest,
    ) -> BoxProviderFuture<'a, Result<BoxProviderStream<'static, Result<wire::Delivery>>>> {
        Box::pin(async { Err(Error::Unsupported("unused".into())) })
    }
    fn observe<'a>(
        &'a self,
        request: wire::ObserveRequest,
    ) -> BoxProviderFuture<'a, Result<wire::OperationStatus>> {
        Box::pin(async move { self.authenticate(&wire_api::validate_observe_request(request)?) })
    }
    fn cancel<'a>(
        &'a self,
        request: wire::CancelRequest,
    ) -> BoxProviderFuture<'a, Result<wire::CancelResponse>> {
        Box::pin(async move {
            let control = wire_api::validate_cancel_request(request)?;
            Ok(wire::CancelResponse {
                status: Some(self.authenticate(&control.control)?),
                operation: Some(wire::OperationIdentity {
                    operation_id: control.control.operation_id.to_string(),
                    idempotency_key: control.idempotency_key.0,
                }),
            })
        })
    }
}

fn cancel(request: wire::ObserveRequest) -> wire::CancelRequest {
    wire::CancelRequest {
        operation_id: request.operation_id,
        protocol: request.protocol,
        owner: request.owner,
        scope: request.scope,
        recursive: false,
        idempotency_key: "retry".into(),
    }
}

#[tokio::test]
async fn control_forwarding_moves_signed_scopes_and_authenticates_once() -> Result<()> {
    let api = ControlProbe::new()?;
    let observe = api.request()?;
    *api.allocation.lock().map_err(storage)? = observe
        .scope
        .as_ref()
        .map(|scope| scope.id.as_ptr() as usize);
    wire_api::observe(&api, observe).await?;
    let cancel = cancel(api.request()?);
    *api.allocation.lock().map_err(storage)? = cancel
        .scope
        .as_ref()
        .map(|scope| scope.id.as_ptr() as usize);
    wire_api::cancel(&api, cancel).await?;
    assert_eq!(api.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn control_forwarding_rejects_status_from_another_owner() -> Result<()> {
    let mut api = ControlProbe::new()?;
    api.wrong_owner = true;
    assert!(matches!(
        wire_api::observe(&api, api.request()?).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        wire_api::cancel(&api, cancel(api.request()?)).await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(api.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn real_socket_controls_authenticate_only_in_the_host() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let service = Arc::new(HarnessService::bind(endpoint.clone())?);
    let api = Arc::new(ControlProbe::new()?);
    service.register("probe".into(), api.clone())?;
    let running = tokio::spawn(service.clone().serve());
    let client = LocalHarnessClient::new(endpoint, "probe".into());
    client.observe(api.request()?).await?;
    client.cancel(cancel(api.request()?)).await?;
    let mut forged = api.request()?;
    forged
        .scope
        .as_mut()
        .ok_or_else(|| Error::Invalid("scope".into()))?
        .proof
        .fill(0);
    assert!(matches!(
        client.observe(forged.clone()).await,
        Err(Error::Unauthorized(_))
    ));
    assert!(matches!(
        client.cancel(cancel(forged)).await,
        Err(Error::Unauthorized(_))
    ));
    assert_eq!(api.calls.load(Ordering::SeqCst), 4);
    service.shutdown().await?;
    running.await.map_err(storage)??;
    Ok(())
}

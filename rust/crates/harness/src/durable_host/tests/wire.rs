use super::*;
use crate::{core::authority_probe, wire, wire_api, wire_codec};

#[tokio::test]
async fn control_requests_verify_the_caller_grant_once() -> Result<()> {
    let f = fixture::fixture().await?;
    let admission = f.admission(11, json!(0));
    let operation_id = admission.operation_id.to_string();
    f.host.admit(admission).await?;
    let scope = f.issuer.issue(
        "wire-control-probe",
        &Capabilities::new(["operation:observe", "operation:cancel"]),
        None,
        None,
        &mut None,
    )?;
    let observe = wire::ObserveRequest {
        protocol: Some(wire_api::current_protocol()),
        owner: Some(wire_codec::encode_authority(f.issuer.audience())),
        operation_id,
        scope: Some(wire::Scope::from(&scope)),
    };
    authority_probe::start(scope.id());
    f.host.wire_observe(observe.clone()).await?;
    assert_eq!(authority_probe::finish(), 1);

    let cancel = wire::CancelRequest {
        operation_id: observe.operation_id.clone(),
        protocol: observe.protocol.clone(),
        owner: observe.owner.clone(),
        scope: observe.scope.clone(),
        recursive: false,
        idempotency_key: "wire-cancel-probe".into(),
    };
    authority_probe::start(scope.id());
    f.host.wire_cancel(cancel.clone()).await?;
    assert_eq!(authority_probe::finish(), 1);
    authority_probe::start(scope.id());
    f.host.wire_cancel(cancel).await?;
    assert_eq!(authority_probe::finish(), 1, "retries authenticate too");

    let mut forged = observe;
    let signed = forged
        .scope
        .as_mut()
        .ok_or_else(|| Error::Invalid("scope".into()))?;
    signed.proof.fill(0);
    authority_probe::start(scope.id());
    assert!(matches!(
        f.host.wire_observe(forged).await,
        Err(Error::Unauthorized(_))
    ));
    assert_eq!(authority_probe::finish(), 1);
    Ok(())
}

use super::*;
use crate::{
    Admission, Error, LocalHarness, LocalHarnessConfig, OperationId, Result,
    conversation::{VolumeClass, VolumeOwner, VolumeRef},
    core::{Action, LifecycleState},
    filesystem::FilesystemHost,
    local::LocalProject,
    model::Model,
    resources::ProviderRef,
    runtime::RuntimeTask,
    test_support::{ScriptedModelProvider, ScriptedTurn},
    wire,
    wire_api::{HarnessWireApi, current_protocol},
};
use futures::StreamExt as _;
use std::{path::Path, sync::Arc};

async fn harness(directory: &Path) -> Result<Arc<LocalHarness>> {
    acyclic_native_runtime::initialize_native_io_services().map_err(storage)?;
    let provider = ProviderRef::new("test".into(), "filesystem".into(), "2".into(), &mut None)?;
    let volume = VolumeRef::new(
        provider.clone(),
        "project".into(),
        VolumeClass::Project,
        VolumeOwner::Project("test".into()),
        &mut None,
    )?;
    let directory = directory.join("filesystem");
    let fs = acyclic_fs::Fs::local(acyclic_fs::LocalOptions::new(&directory))
        .await
        .map_err(storage)?;
    FilesystemHost::new(fs, &provider, None)?
        .create_volume(&volume)
        .await?;
    let model = Model::new(
        "scripted".into(),
        "test".into(),
        "1".into(),
        serde_json::Value::Null,
    )?;
    let provider = Arc::new(ScriptedModelProvider::new([ScriptedTurn::Text(
        "done".into(),
    )]));
    Ok(Arc::new(
        LocalHarness::open(LocalHarnessConfig::new(
            LocalProject::Volume { directory, volume },
            (model, provider),
        ))
        .await?,
    ))
}

fn handshake() -> wire::HandshakeRequest {
    wire::HandshakeRequest {
        protocol: Some(current_protocol()),
        required: None,
    }
}

fn command(
    harness: &LocalHarness,
    revision: u64,
    to: LifecycleState,
) -> Result<wire::CommandEnvelope> {
    let (authority, scope) = harness.wire_owner();
    let action = Action::TransitionLifecycle { to, reason: None };
    Ok(wire::CommandEnvelope {
        protocol: Some(current_protocol()),
        authority: Some(authority),
        scope: Some(scope),
        operation: Some(wire::OperationIdentity {
            operation_id: OperationId::new().to_string(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        }),
        expected_revision: revision,
        action_type: action.tag().into(),
        canonical_action_json: crate::contract::canonical_json_bytes(&action)?,
        ..Default::default()
    })
}

async fn next(
    stream: &mut acyclic_stream::BoxProviderStream<'static, Result<wire::Delivery>>,
) -> Result<wire::Delivery> {
    tokio::time::timeout(std::time::Duration::from_secs(3), stream.next())
        .await
        .map_err(storage)?
        .ok_or_else(|| Error::NotFound("delivery".into()))?
}

#[tokio::test]
async fn real_socket_routes_wire_calls_and_preserves_service_after_client_exit() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let service = Arc::new(HarnessService::bind(endpoint.clone())?);
    assert!(matches!(
        HarnessService::bind(endpoint.clone()),
        Err(Error::Conflict(_))
    ));
    let host = harness(&directory.path().join("one")).await?;
    service.register("one".into(), host.clone()).await?;
    assert!(matches!(
        service.register("one".into(), host.clone()).await,
        Err(Error::Conflict(_))
    ));
    let server = tokio::spawn(service.clone().serve());
    let client = LocalHarnessClient::new(endpoint.clone(), "one".into());
    assert_eq!(
        client.handshake(handshake()).await?.protocol,
        Some(current_protocol())
    );
    assert!(matches!(
        client.handshake(wire::HandshakeRequest::default()).await,
        Err(Error::Invalid(_))
    ));
    let parallel = futures::future::join_all((0..32).map(|_| client.handshake(handshake()))).await;
    for response in parallel {
        assert_eq!(response?.protocol, Some(current_protocol()));
    }
    let first = command(&host, 0, LifecycleState::Active)?;
    assert_eq!(
        client.submit(first.clone()).await?.state,
        wire::AdmissionState::Accepted as i32
    );
    assert_eq!(
        client.submit(first).await?.state,
        wire::AdmissionState::Accepted as i32
    );
    let mut replay = client
        .replay(wire::ResumeRequest {
            protocol: Some(current_protocol()),
            cursors: vec![host.start_cursor()],
        })
        .await?;
    let historical = next(&mut replay).await?;
    assert_eq!(
        (historical.from_revision, historical.through_revision),
        (0, 1)
    );
    assert_eq!(historical.events.len(), 1);
    assert!(!historical.live);
    let second = command(&host, 1, LifecycleState::Waiting)?;
    assert_eq!(
        client.submit(second).await?.state,
        wire::AdmissionState::Accepted as i32
    );
    let live = next(&mut replay).await?;
    assert_eq!((live.from_revision, live.through_revision), (1, 2));
    assert!(live.live);
    drop(replay);
    drop(client);
    let client = LocalHarnessClient::new(endpoint.clone(), "one".into());
    assert_eq!(
        client.handshake(handshake()).await?.protocol,
        Some(current_protocol())
    );
    let mut cursor = host.start_cursor();
    cursor.revision = 1;
    let mut replay = client
        .replay(wire::ResumeRequest {
            protocol: Some(current_protocol()),
            cursors: vec![cursor],
        })
        .await?;
    assert_eq!(next(&mut replay).await?.through_revision, 2);
    drop(replay);
    let missing = LocalHarnessClient::new(endpoint.clone(), "missing".into());
    assert!(matches!(
        missing.handshake(handshake()).await,
        Err(Error::NotFound(_))
    ));
    server.abort();
    let _ = server.await;
    drop(service);
    drop(HarnessService::bind(endpoint)?);
    Ok(())
}

#[tokio::test]
async fn real_socket_observes_scripted_task_and_authenticates_cancellation() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let service = Arc::new(HarnessService::bind(endpoint.clone())?);
    let host = harness(&directory.path().join("one")).await?;
    let other = harness(&directory.path().join("two")).await?;
    service.register("one".into(), host.clone()).await?;
    service.register("two".into(), other.clone()).await?;
    let server = tokio::spawn(service.serve());
    let client = LocalHarnessClient::new(endpoint.clone(), "one".into());
    let operation = OperationId::new();
    let Admission::Accepted(RuntimeTask::Durable { task_id, .. }) =
        host.submit(operation, "finish").await?
    else {
        return Err(Error::Conflict("task admission".into()));
    };
    let (owner, scope) = host.wire_owner();
    let request = wire::ObserveRequest {
        operation_id: task_id.to_string(),
        protocol: Some(current_protocol()),
        owner: Some(owner.clone()),
        scope: Some(scope.clone()),
    };
    assert_eq!(
        client.observe(request.clone()).await?.state,
        wire::CompletionState::Running as i32
    );
    assert_eq!(host.resume(task_id).await?.text, "done");
    assert_eq!(
        client.observe(request.clone()).await?.state,
        wire::CompletionState::Succeeded as i32
    );
    let wrong_workspace = LocalHarnessClient::new(endpoint, "two".into());
    assert!(matches!(
        wrong_workspace.observe(request.clone()).await,
        Err(Error::Unauthorized(_))
    ));
    let mut forged = request;
    forged
        .scope
        .as_mut()
        .ok_or_else(|| Error::Invalid("scope".into()))?
        .proof
        .fill(0);
    assert!(matches!(
        client.observe(forged).await,
        Err(Error::Unauthorized(_))
    ));
    let Admission::Accepted(RuntimeTask::Durable { task_id, .. }) =
        host.submit(OperationId::new(), "cancel").await?
    else {
        return Err(Error::Conflict("task admission".into()));
    };
    let request = wire::CancelRequest {
        operation_id: task_id.to_string(),
        protocol: Some(current_protocol()),
        owner: Some(owner),
        scope: Some(scope),
        recursive: true,
        idempotency_key: "cancel-once".into(),
    };
    let first = client.cancel(request.clone()).await?;
    let second = client.cancel(request).await?;
    assert_eq!(first.operation, second.operation);
    assert!(
        second
            .status
            .ok_or_else(|| Error::Invalid("status".into()))?
            .cancellation_requested
    );
    server.abort();
    let _ = server.await;
    Ok(())
}

//! Real local-socket scenarios over durable workspace hosts.
use acyclic_harness as harness;
use futures::StreamExt as _;
use harness::{
    Admission, Error, LocalHarness, OperationId, Result,
    core::{Action, LifecycleState},
    runtime::RuntimeTask,
    service::{HarnessService, LocalHarnessClient, ServiceEndpoint},
    wire,
    wire_api::{HarnessWireApi, current_protocol},
};
use prost::Message as _;
use std::sync::Arc;
#[path = "service/control.rs"]
mod control;
#[path = "service/lifecycle.rs"]
mod lifecycle;
#[path = "service/paused.rs"]
mod paused;
#[path = "service/support.rs"]
mod support;
fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
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
    reason: Option<String>,
) -> Result<wire::CommandEnvelope> {
    let (owner, scope) = harness.owner();
    harness::wire_codec::encode_command_envelope(
        owner,
        &harness::core::Command {
            operation_id: OperationId::new(),
            idempotency_key: harness::IdempotencyKey::new(uuid::Uuid::new_v4().to_string())?,
            expected_revision: revision,
            scope: scope.clone(),
            causal_parent: None,
            action: Action::TransitionLifecycle { to, reason },
        },
    )
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
    let service = Arc::new(
        HarnessService::bind(endpoint.clone())
            .map_err(|e| Error::Storage(format!("initial bind: {e}")))?,
    );
    assert!(matches!(
        HarnessService::bind(endpoint.clone()),
        Err(Error::Conflict(_))
    ));
    let host = support::open(&directory.path().join("one")).await?;
    service.register("one".into(), host.clone())?;
    assert!(matches!(
        service.register("one".into(), host.clone()),
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
        assert_eq!(
            response
                .map_err(|e| Error::Storage(format!("parallel handshake: {e}")))?
                .protocol,
            Some(current_protocol())
        );
    }
    let first = command(&host, 0, LifecycleState::Active, None)?;
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
    let second = command(&host, 1, LifecycleState::Waiting, None)?;
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
    let idle_replay = client
        .replay(wire::ResumeRequest {
            protocol: Some(current_protocol()),
            cursors: vec![wire::ReplayCursor {
                revision: 2,
                ..host.start_cursor()
            }],
        })
        .await?;
    client.handshake(handshake()).await?;
    let (first, second) = tokio::join!(service.shutdown(), service.shutdown());
    first?;
    second?;
    server.await.map_err(storage)??;
    for _ in 0..3 {
        let restarted = Arc::new(HarnessService::bind(endpoint.clone())?);
        restarted.register("one".into(), host.clone())?;
        let server = tokio::spawn(restarted.clone().serve());
        assert_eq!(
            client.handshake(handshake()).await?.protocol,
            Some(current_protocol())
        );
        restarted.shutdown().await?;
        server.await.map_err(storage)??;
    }
    drop(idle_replay);
    Ok(())
}

#[tokio::test]
async fn real_socket_observes_scripted_task_and_authenticates_cancellation() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let service = Arc::new(
        HarnessService::bind(endpoint.clone())
            .map_err(|e| Error::Storage(format!("initial bind: {e}")))?,
    );
    let host = support::open(&directory.path().join("one")).await?;
    let other = support::open(&directory.path().join("two")).await?;
    service.register("one".into(), host.clone())?;
    service.register("two".into(), other.clone())?;
    let server = tokio::spawn(service.clone().serve());
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
    let status = second
        .status
        .ok_or_else(|| Error::Invalid("status".into()))?;
    assert_eq!(status.state, wire::CompletionState::Cancelled as i32);
    assert!(!status.cancellation_requested);
    service.shutdown().await?;
    server.await.map_err(storage)??;
    Ok(())
}

#[tokio::test]
async fn real_socket_replays_an_authenticated_maximum_size_record() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let host = support::open(&directory.path().join("workspace")).await?;
    let first = command(&host, 0, LifecycleState::Active, None)?;
    assert_eq!(
        HarnessWireApi::submit(host.as_ref(), first).await?.state,
        wire::AdmissionState::Accepted as i32
    );
    let mut history = host
        .replay(wire::ResumeRequest {
            protocol: Some(current_protocol()),
            cursors: vec![host.start_cursor()],
        })
        .await?;
    let mut expected = next(&mut history)
        .await?
        .events
        .into_iter()
        .next()
        .ok_or_else(|| Error::Invalid("history event".into()))?;
    drop(history);
    expected.revision = 2;
    let mut padding = 0;
    for _ in 0..3 {
        let payload = harness::core::EventPayload::LifecycleTransitioned {
            from: LifecycleState::Active,
            to: LifecycleState::Waiting,
            reason: Some("x".repeat(padding)),
        };
        expected.canonical_payload_json = serde_json::to_vec(&payload).map_err(storage)?;
        let size = expected.encoded_len();
        if size == acyclic_stream::MAX_RECORD_BYTES {
            break;
        }
        padding = (padding + acyclic_stream::MAX_RECORD_BYTES)
            .checked_sub(size)
            .ok_or_else(|| Error::Invalid("record padding underflows".into()))?;
    }
    assert_eq!(expected.encoded_len(), acyclic_stream::MAX_RECORD_BYTES);
    let maximum = command(&host, 1, LifecycleState::Waiting, Some("x".repeat(padding)))?;
    assert_eq!(
        HarnessWireApi::submit(host.as_ref(), maximum).await?.state,
        wire::AdmissionState::Accepted as i32
    );
    let service = Arc::new(HarnessService::bind(endpoint.clone())?);
    service.register("workspace".into(), host.clone())?;
    let server = tokio::spawn(service.clone().serve());
    let client = LocalHarnessClient::new(endpoint, "workspace".into());
    let mut replay = client
        .replay(wire::ResumeRequest {
            protocol: Some(current_protocol()),
            cursors: vec![wire::ReplayCursor {
                revision: 1,
                ..host.start_cursor()
            }],
        })
        .await?;
    let delivery = next(&mut replay).await?;
    assert_eq!((delivery.from_revision, delivery.through_revision), (1, 2));
    let [event] = delivery.events.as_slice() else {
        return Err(Error::Invalid("single-event delivery".into()));
    };
    assert_eq!(event.encoded_len(), acyclic_stream::MAX_RECORD_BYTES);
    assert!(delivery.encoded_len() > acyclic_stream::MAX_RECORD_BYTES);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&event.canonical_payload_json)
            .map_err(storage)?,
        serde_json::from_slice::<serde_json::Value>(&expected.canonical_payload_json)
            .map_err(storage)?,
    );
    service.shutdown().await?;
    server.await.map_err(storage)??;
    Ok(())
}

#[tokio::test]
async fn real_socket_requests_cancellation_of_running_work_without_settling_its_lease() -> Result<()>
{
    let directory = tempfile::tempdir().map_err(storage)?;
    let endpoint = ServiceEndpoint::in_directory(directory.path().join("service"))?;
    let provider = Arc::new(paused::PausedModel::new());
    let host =
        support::open_with_provider(&directory.path().join("workspace"), provider.clone()).await?;
    let service = Arc::new(HarnessService::bind(endpoint.clone())?);
    service.register("workspace".into(), host.clone())?;
    let server = tokio::spawn(service.clone().serve());
    let Admission::Accepted(RuntimeTask::Durable { task_id, .. }) = host
        .submit(OperationId::new(), "wait for cancellation")
        .await?
    else {
        return Err(Error::Conflict("task admission".into()));
    };
    let running = host.clone();
    let worker = tokio::spawn(async move { running.resume(task_id).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        provider.started.notified(),
    )
    .await
    .map_err(storage)?;
    assert_eq!(provider.script.dispatches().len(), 1);
    let (owner, scope) = host.wire_owner();
    let request = wire::CancelRequest {
        operation_id: task_id.to_string(),
        protocol: Some(current_protocol()),
        owner: Some(owner),
        scope: Some(scope),
        recursive: true,
        idempotency_key: "running-cancel".into(),
    };
    let client = LocalHarnessClient::new(endpoint, "workspace".into());
    let first = client.cancel(request.clone()).await?;
    let second = client.cancel(request).await?;
    assert_eq!(first.operation, second.operation);
    let status = second
        .status
        .ok_or_else(|| Error::Invalid("status".into()))?;
    assert_eq!(status.state, wire::CompletionState::Running as i32);
    assert!(status.cancellation_requested);
    worker.abort();
    let _ = worker.await;
    service.shutdown().await?;
    server.await.map_err(storage)??;
    Ok(())
}

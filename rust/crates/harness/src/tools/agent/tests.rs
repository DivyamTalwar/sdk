use super::*;
use crate::{
    AgentId, Capabilities, OperationId, TaskId,
    conversation::{VolumeClass, VolumeOperation, VolumeOwner, VolumeRef},
    core::{AggregateKind, Authority, AuthorityIssuer},
    resources::ProviderRef,
};
use acyclic_stream::MemoryStream;
use proptest::prelude::*;

async fn files() -> TaskFiles<MemoryStream> {
    let agent = AgentId::from_bytes([1; 16]);
    let volume = VolumeRef::new(
        ProviderRef::new("local".into(), "filesystem".into(), "2".into(), &mut None).unwrap(),
        "agent-tool".into(),
        VolumeClass::AgentPrivate,
        VolumeOwner::Agent(agent),
        &mut None,
    )
    .unwrap();
    let issuer = AuthorityIssuer::new(
        "agent-tool",
        [1; 32],
        Authority {
            kind: AggregateKind::Task,
            id: TaskId::from_bytes([2; 16]).to_string(),
        },
    );
    let grants = [VolumeOperation::Read, VolumeOperation::Write]
        .map(|operation| volume.capability(operation, &mut None).unwrap());
    let scope = issuer
        .issue(
            "owner",
            &Capabilities::new(grants),
            Some(agent),
            None,
            &mut None,
        )
        .unwrap();
    TaskFiles::open(
        Arc::new(MemoryStream::default()),
        &volume,
        &issuer.verifier(&mut None).unwrap(),
        &scope,
        4096,
    )
    .await
    .unwrap()
}

async fn call(
    files: &TaskFiles<MemoryStream>,
    command: Value,
    operation: OperationId,
) -> Result<Value> {
    access(
        files,
        serde_json::from_value(command).unwrap(),
        operation,
        16,
        false,
    )
    .await
    .map(|result| result.unwrap())
}

#[tokio::test]
async fn spawn_and_send_replay_the_same_files_and_archive_remains_inspectable() {
    let files = files().await;
    let operation = OperationId::from_bytes([3; 16]);
    let spawn = json!({"command":"spawn","reference":"worker","prompt":"work"});
    for _ in 0..2 {
        call(&files, spawn.clone(), operation).await.unwrap();
    }
    let send = json!({"command":"send","reference":"worker","message":"more work"});
    for _ in 0..2 {
        call(&files, send.clone(), operation).await.unwrap();
    }
    let snapshot = files
        .observe(Query::Children, None, 16)
        .await
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(snapshot.files.len(), 2);
    assert!(snapshot.files.iter().any(
        |file| file.key.path() == "agents/worker/request.md" && file.bytes.as_ref() == b"work"
    ));
    assert!(
        call(
            &files,
            json!({"command":"send","reference":"worker","message":"changed"}),
            operation
        )
        .await
        .is_err()
    );
    let archived = call(
        &files,
        json!({"command":"control","reference":"worker","action":"archive"}),
        operation,
    )
    .await
    .unwrap();
    assert_eq!(archived["state"], json!(ChildState::Archived));
    assert_eq!(
        call(
            &files,
            json!({"command":"wait","reference":"worker"}),
            operation
        )
        .await
        .unwrap(),
        archived
    );
    assert_eq!(
        call(
            &files,
            json!({"command":"status","reference":"worker"}),
            operation
        )
        .await
        .unwrap(),
        archived
    );
    assert_eq!(
        call(&files, json!({"command":"list"}), operation)
            .await
            .unwrap(),
        json!({"agents":[]})
    );
    assert!(
        call(
            &files,
            json!({"command":"control","reference":"worker","action":"resume"}),
            operation
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn wait_follows_the_existing_snapshot_cut_and_sees_control_without_polling() {
    let files = files().await;
    let operation = OperationId::from_bytes([3; 16]);
    call(
        &files,
        json!({"command":"spawn","reference":"worker","prompt":"work"}),
        operation,
    )
    .await
    .unwrap();
    let mut waiter = Box::pin(call(
        &files,
        json!({"command":"wait","reference":"worker"}),
        operation,
    ));
    assert!(matches!(
        futures::poll!(&mut waiter),
        std::task::Poll::Pending
    ));
    call(
        &files,
        json!({"command":"control","reference":"worker","action":"interrupt"}),
        operation,
    )
    .await
    .unwrap();
    let result = waiter.await.unwrap();
    assert_eq!(result["state"], json!(ChildState::Interrupted));
    assert_eq!(result["settled"], true);
}

#[tokio::test]
async fn reconciliation_does_not_reapply_a_superseded_control() {
    let files = files().await;
    let operation = OperationId::from_bytes([3; 16]);
    call(
        &files,
        json!({"command":"spawn","reference":"worker","prompt":"work"}),
        operation,
    )
    .await
    .unwrap();
    let pause = json!({"command":"control","reference":"worker","action":"pause"});
    call(&files, pause.clone(), operation).await.unwrap();
    let paused = access(
        &files,
        serde_json::from_value(pause.clone()).unwrap(),
        operation,
        16,
        true,
    )
    .await
    .unwrap();
    assert!(paused.is_some());
    call(
        &files,
        json!({"command":"control","reference":"worker","action":"resume"}),
        operation,
    )
    .await
    .unwrap();
    let before = files
        .observe(Query::Children, None, 16)
        .await
        .unwrap()
        .snapshot
        .unwrap()
        .generation;
    assert!(
        access(
            &files,
            serde_json::from_value(pause).unwrap(),
            operation,
            16,
            true
        )
        .await
        .unwrap()
        .is_none()
    );
    let after = files
        .observe(Query::Children, None, 16)
        .await
        .unwrap()
        .snapshot
        .unwrap()
        .generation;
    assert_eq!(before, after);
    assert_eq!(
        call(
            &files,
            json!({"command":"status","reference":"worker"}),
            operation
        )
        .await
        .unwrap()["state"],
        json!(ChildState::Running)
    );
}

#[tokio::test]
async fn new_inbox_files_make_a_previously_settled_status_pending() {
    let files = files().await;
    let operation = OperationId::from_bytes([3; 16]);
    call(
        &files,
        json!({"command":"spawn","reference":"worker","prompt":"work"}),
        operation,
    )
    .await
    .unwrap();
    let snapshot = files
        .observe(Query::Children, None, 16)
        .await
        .unwrap()
        .snapshot
        .unwrap();
    files
        .inner
        .append(
            snapshot.generation,
            &FileValue::new(
                FileKey::Child {
                    reference: Component::new("worker").unwrap(),
                    file: ChildFile::Status,
                },
                serde_json::to_vec(&json!({"settled":true,"inputs":1,"text":"done"})).unwrap(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        call(
            &files,
            json!({"command":"wait","reference":"worker"}),
            operation
        )
        .await
        .unwrap()["settled"],
        true
    );
    call(
        &files,
        json!({"command":"send","reference":"worker","message":"more work"}),
        operation,
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &files,
            json!({"command":"status","reference":"worker"}),
            operation
        )
        .await
        .unwrap()["settled"],
        false
    );
    let mut waiter = Box::pin(call(
        &files,
        json!({"command":"wait","reference":"worker"}),
        operation,
    ));
    assert!(matches!(
        futures::poll!(&mut waiter),
        std::task::Poll::Pending
    ));
    call(
        &files,
        json!({"command":"control","reference":"worker","action":"cancel"}),
        operation,
    )
    .await
    .unwrap();
    assert_eq!(waiter.await.unwrap()["state"], json!(ChildState::Cancelled));
}

proptest! {
    #[test]
    fn tool_arguments_round_trip(reference in "[a-z][a-z0-9_-]{0,20}", body in ".{1,100}") {
        let reference = Component::new(reference).unwrap();
        for command in [
            AgentCommand::Spawn { reference: reference.clone(), prompt: body.clone(), model: None, tools: Some(vec!["read".into()]) },
            AgentCommand::Send { reference: reference.clone(), message: body },
            AgentCommand::Control { reference: reference.clone(), action: Control::Archive },
            AgentCommand::Wait { reference: reference.clone() },
            AgentCommand::Status { reference }, AgentCommand::List,
        ] {
            prop_assert_eq!(serde_json::from_slice::<AgentCommand>(&serde_json::to_vec(&command).unwrap()).unwrap(), command);
        }
    }
}

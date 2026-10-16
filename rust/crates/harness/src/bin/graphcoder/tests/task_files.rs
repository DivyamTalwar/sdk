use super::*;
use acyclic_harness::{
    client::TaskFileClient,
    task_volume::{Channel, Component, FileKey, FileValue, Query, TaskFile, TaskVolume},
};
use acyclic_stream::{MemoryStream, StreamPath};
use std::sync::Arc;

#[tokio::test]
async fn followed_task_files_render_without_a_wire_connection() {
    let files = TaskVolume::open(
        Arc::new(MemoryStream::default()),
        StreamPath::new("composer-files").unwrap(),
    )
    .await
    .unwrap();
    let task = acyclic_harness::TaskId::from_bytes([6; 16]);
    let owner = wire::Authority {
        kind: wire::AggregateKind::Task as i32,
        id: task.to_string(),
    };
    let cut = files.snapshot(&Query::Tasks, 16).await.unwrap();
    let mut client = TaskFileClient::observe(&files, &owner, 16, 4096)
        .await
        .unwrap();
    let mut renderer = ClientRenderer::default();
    renderer.update(client.views(), &owner);
    assert!(!picture(&renderer).contains("File-backed answer"));
    files
        .append(
            cut.generation,
            &FileValue {
                key: FileKey::Task {
                    task: Component::new(task.to_string()).unwrap(),
                    file: TaskFile::Chunk {
                        turn: 0,
                        sequence: 0,
                    },
                },
                channel: Some(Channel::Text),
                bytes: b"**File-backed answer**".to_vec().into(),
            },
        )
        .await
        .unwrap();
    client.next_change().await.unwrap();
    renderer.update(client.views(), &owner);
    assert!(picture(&renderer).contains("File-backed answer"));
    assert!(!picture(&renderer).contains("**File-backed answer**"));
}

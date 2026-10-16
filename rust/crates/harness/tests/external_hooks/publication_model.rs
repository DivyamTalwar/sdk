use super::{boundary_stream::BoundaryStream, *};
use acyclic_harness::task_volume::{Component, FileKey, FileValue, TaskFile, TaskVolume};
use acyclic_stream::{AppendRequest, CommitCondition, MemoryStream, StreamPath, StreamProvider};
use proptest::prelude::*;
use std::sync::Arc;

// Compile the exact production publication path into the model. The provider's
// conditional commit is the atomic interface; no Tokio mutex is a modeled lock.
#[path = "../../src/external_executor/publication.rs"]
mod publication;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    // Property: retrying the exact immutable result performs no append; changed
    // bytes under the same request identity cannot replace the original result.
    #[test]
    fn result_retry_is_immutable(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().map_err(|error| storage(error))?;
        runtime.block_on(async move {
            let provider = Arc::new(MemoryStream::default());
            let volume = TaskVolume::open(provider, StreamPath::new("results").map_err(|error| storage(error))?)
                .await.map_err(|error| storage(error))?;
            let file = FileValue::new(FileKey::Task {
                task:Component::new("task").map_err(|error| storage(error))?,
                file:TaskFile::RequestResult(Component::new("result").map_err(|error| storage(error))?),
            }, bytes.clone());
            let condition = CommitCondition::Absent {path:StreamPath::new("unused-owner").map_err(|error| storage(error))?};
            publication::publish(&volume, &file, condition.clone(), None).await?;
            let query = acyclic_harness::task_volume::Query::File(file.key.clone());
            let before = volume.snapshot(&query, 1).await.map_err(|error| storage(error))?;
            publication::publish(&volume, &file, condition.clone(), None).await?;
            let mut changed = bytes;
            changed.push(0);
            assert!(matches!(publication::publish(&volume, &FileValue::new(file.key.clone(), changed), condition, None).await,
                Err(Error::Conflict(_))));
            let after = volume.snapshot(&query, 1).await.map_err(|error| storage(error))?;
            assert_eq!(before.generation, after.generation);
            assert_eq!(after.files, vec![file]);
            Ok::<_, Error>(())
        })?;
    }
}

#[test]
fn result_publication_is_atomic_with_task_ownership_loss() {
    model(false);
}

#[test]
#[should_panic(expected = "stale task owner published a result")]
fn negative_control_detects_dropping_the_owner_commit_condition() {
    model(true);
}

fn model(broken: bool) {
    shuttle::check_dfs(
        move || {
            let result: std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> =
                shuttle::future::block_on(async move {
                    let inner = Arc::new(MemoryStream::default());
                    let owner = StreamPath::new("owner")?;
                    inner
                        .append(AppendRequest {
                            path: owner.clone(),
                            records: vec![b"owner".to_vec().into()],
                            if_tail: None,
                            idempotency_key: None,
                        })
                        .await?;
                    let provider = Arc::new(BoundaryStream {
                        inner: inner.clone(),
                        broken_follow: false,
                        owner: Some((owner.clone(), 1)),
                        broken_fence: broken,
                        lose_result_ack: false,
                    });
                    let volume = TaskVolume::open(provider, StreamPath::new("results")?).await?;
                    let file = FileValue::new(
                        FileKey::Task {
                            task: Component::new("task")?,
                            file: TaskFile::RequestResult(Component::new("request")?),
                        },
                        b"{}".to_vec(),
                    );
                    let condition = CommitCondition::Tail {
                        path: owner.clone(),
                        expected: 1,
                    };
                    let publisher = shuttle::future::spawn(async move {
                        publication::publish(&volume, &file, condition, None).await
                    });
                    let invalidate = shuttle::future::spawn(async move {
                        shuttle::future::yield_now().await;
                        inner
                            .append(AppendRequest {
                                path: owner,
                                records: vec![b"replacement".to_vec().into()],
                                if_tail: None,
                                idempotency_key: None,
                            })
                            .await
                    });
                    let published = publisher
                        .await
                        .map_err(|error| format!("publisher: {error}"))?;
                    invalidate
                        .await
                        .map_err(|error| format!("invalidation: {error}"))??;
                    // Either publication wins atomically or the stale fence rejects it.
                    assert!(published.is_ok() || matches!(published, Err(Error::Storage(_))));
                    Ok(())
                });
            assert!(result.is_ok(), "publication model failed: {result:?}");
        },
        None,
    );
}

#[test]
fn lost_result_acknowledgement_reuses_the_accepted_file_without_another_append() {
    shuttle::check_dfs(
        || {
            let result: std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> =
                shuttle::future::block_on(async {
                    let inner = Arc::new(MemoryStream::default());
                    let owner = StreamPath::new("owner")?;
                    inner
                        .append(AppendRequest {
                            path: owner.clone(),
                            records: vec![b"owner".to_vec().into()],
                            if_tail: None,
                            idempotency_key: None,
                        })
                        .await?;
                    let provider = Arc::new(BoundaryStream {
                        inner,
                        broken_follow: false,
                        owner: Some((owner.clone(), 1)),
                        broken_fence: false,
                        lose_result_ack: true,
                    });
                    let volume = TaskVolume::open(provider, StreamPath::new("results")?).await?;
                    let file = FileValue::new(
                        FileKey::Task {
                            task: Component::new("task")?,
                            file: TaskFile::RequestResult(Component::new("request")?),
                        },
                        b"{}".to_vec(),
                    );
                    let condition = CommitCondition::Tail {
                        path: owner,
                        expected: 1,
                    };
                    assert!(matches!(
                        publication::publish(&volume, &file, condition.clone(), None).await,
                        Err(Error::Storage(_))
                    ));
                    let query = acyclic_harness::task_volume::Query::File(file.key.clone());
                    let before = volume.snapshot(&query, 1).await?;
                    assert_eq!(before.files, vec![file.clone()]);
                    publication::publish(&volume, &file, condition, None).await?;
                    let after = volume.snapshot(&query, 1).await?;
                    assert_eq!(before.generation, after.generation);
                    assert_eq!(before.files, after.files);
                    Ok(())
                });
            assert!(result.is_ok(), "lost result acknowledgement: {result:?}");
        },
        None,
    );
}

fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

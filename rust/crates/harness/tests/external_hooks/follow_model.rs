//! Model the client at the provider's atomic snapshot/follow handoff. Provider
//! operations are contract-atomic; this does not model dependency-internal locks.
use acyclic_harness::{
    external_executor::{ExternalHost, request},
    task_volume::{Component, Query, TaskVolume},
};
use acyclic_stream::*;
use futures::StreamExt as _;
use serde_json::json;
use std::sync::Arc;

struct Follower {
    inner: Arc<MemoryStream>,
    broken: bool,
}

#[async_trait::async_trait]
impl StreamProvider for Follower {
    async fn inspect_idempotency(
        &self,
        key: IdempotencyKey,
    ) -> Result<Option<IdempotencyObservation>, StreamError> {
        self.inner.inspect_idempotency(key).await
    }
    async fn tail(&self, path: StreamPath) -> Result<u64, StreamError> {
        self.inner.tail(path).await
    }
    async fn bounds(&self, path: StreamPath) -> Result<StreamBounds, StreamError> {
        self.inner.bounds(path).await
    }
    async fn watermarks(&self, path: StreamPath) -> Result<StreamWatermarks, StreamError> {
        self.inner.watermarks(path).await
    }
    async fn append(&self, request: AppendRequest) -> Result<AppendOutcome, StreamError> {
        self.inner.append(request).await
    }
    async fn fork(&self, request: ForkRequest) -> Result<ForkReceipt, StreamError> {
        self.inner.fork(request).await
    }
    async fn read(&self, request: ReadRequest) -> Result<RecordStream, StreamError> {
        self.inner.read(request).await
    }
    async fn follow(&self, path: StreamPath, from: u64) -> Result<RecordStream, StreamError> {
        self.inner.follow(path, from).await
    }
    async fn follow_checked(
        &self,
        path: StreamPath,
        from: u64,
        preceding: CommitId,
    ) -> Result<RecordStream, StreamError> {
        // Every legal order: publish before the snapshot, at this handoff, or
        // after subscription. No artificial sleeps or polling are involved.
        shuttle::future::yield_now().await;
        if self.broken {
            self.inner
                .follow(path.clone(), self.inner.tail(path).await?)
                .await
        } else {
            self.inner.follow_checked(path, from, preceding).await
        }
    }
    async fn children(&self, request: ChildrenRequest) -> Result<ChildStream, StreamError> {
        self.inner.children(request).await
    }
    async fn commit(&self, request: CommitRequest) -> Result<CommitOutcome, StreamError> {
        self.inner.commit(request).await
    }
    async fn read_commit(&self, commit: CommitId) -> Result<CommittedEnvelope, StreamError> {
        self.inner.read_commit(commit).await
    }
}

#[test]
fn client_never_loses_an_answer_at_the_snapshot_follow_handoff() {
    model(false);
}

#[test]
#[should_panic(expected = "deadlock")]
fn negative_control_detects_following_the_new_tail_instead_of_the_snapshot_cursor() {
    model(true);
}

fn model(broken: bool) {
    shuttle::check_dfs(
        move || {
            let result: Result<(), Box<dyn std::error::Error + Send + Sync>> =
                shuttle::future::block_on(async move {
                    let provider = Arc::new(MemoryStream::default());
                    let requests = Arc::new(
                        TaskVolume::open(provider.clone(), StreamPath::new("requests")?).await?,
                    );
                    let results = Arc::new(
                        TaskVolume::open(provider.clone(), StreamPath::new("results")?).await?,
                    );
                    let client_results = TaskVolume::open(
                        Arc::new(Follower {
                            inner: provider,
                            broken,
                        }),
                        StreamPath::new("results")?,
                    )
                    .await?;
                    let task = Component::new("task")?;
                    let mut incoming = requests
                        .observe(Query::Requests(task.clone()), None, 1)
                        .await?
                        .changes;
                    let client_requests = requests.clone();
                    let client_task = task.clone();
                    let client = shuttle::future::spawn(async move {
                        let request = request(
                            ExternalHost::Codex,
                            "SessionEnd",
                            json!({"session_id":"session"}),
                        )?;
                        let call = request.send(&client_requests, &client_results, client_task);
                        assert!(std::mem::size_of_val(&call) < 16 * 1024);
                        assert_eq!(call.await?, json!({"detached":true}));
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                    });
                    let actor = shuttle::future::spawn(async move {
                        let file = incoming
                            .next()
                            .await
                            .ok_or("no request")??
                            .file
                            .ok_or("no request file")?;
                        let request =
                            acyclic_harness::external_executor::ExternalRequest::from_file(
                                &task, &file,
                            )?;
                        shuttle::future::yield_now().await;
                        let answer = request.result_file(task, &json!({"detached":true}))?;
                        let snapshot = results
                            .snapshot(&Query::File(answer.key.clone()), 1)
                            .await?;
                        results.append(snapshot.generation, &answer).await?;
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                    });
                    client.await.map_err(|error| format!("client: {error}"))??;
                    actor.await.map_err(|error| format!("actor: {error}"))??;
                    Ok(())
                });
            assert!(result.is_ok(), "model failed: {result:?}");
        },
        None,
    );
}

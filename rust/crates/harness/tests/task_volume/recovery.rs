use super::*;
use acyclic_stream::{
    AppendOutcome, AppendRequest, ChildStream, ChildrenRequest, CommitId, CommitOutcome,
    CommitRequest, CommittedEnvelope, ForkReceipt, ForkRequest, IdempotencyKey,
    IdempotencyObservation, RecordStream, StreamBounds, StreamError,
};

type StreamResult<T> = std::result::Result<T, StreamError>;

struct Restartable(std::sync::RwLock<Arc<MemoryStream>>);

impl Restartable {
    fn current(&self) -> StreamResult<Arc<MemoryStream>> {
        self.0
            .read()
            .map(|provider| provider.clone())
            .map_err(|_| StreamError::Unavailable)
    }
}

#[async_trait::async_trait]
impl StreamProvider for Restartable {
    async fn inspect_idempotency(
        &self,
        key: IdempotencyKey,
    ) -> StreamResult<Option<IdempotencyObservation>> {
        self.current()?.inspect_idempotency(key).await
    }
    async fn tail(&self, path: StreamPath) -> StreamResult<u64> {
        self.current()?.tail(path).await
    }
    async fn bounds(&self, path: StreamPath) -> StreamResult<StreamBounds> {
        self.current()?.bounds(path).await
    }
    async fn append(&self, request: AppendRequest) -> StreamResult<AppendOutcome> {
        self.current()?.append(request).await
    }
    async fn fork(&self, request: ForkRequest) -> StreamResult<ForkReceipt> {
        self.current()?.fork(request).await
    }
    async fn read(&self, request: ReadRequest) -> StreamResult<RecordStream> {
        self.current()?.read(request).await
    }
    async fn follow(&self, path: StreamPath, from: u64) -> StreamResult<RecordStream> {
        self.current()?.follow(path, from).await
    }
    async fn children(&self, request: ChildrenRequest) -> StreamResult<ChildStream> {
        self.current()?.children(request).await
    }
    async fn read_commit(&self, id: CommitId) -> StreamResult<CommittedEnvelope> {
        self.current()?.read_commit(id).await
    }
    async fn commit(&self, request: CommitRequest) -> StreamResult<CommitOutcome> {
        self.current()?.commit(request).await
    }
}

#[tokio::test]
async fn existing_empty_forks_initialize_and_racing_openers_share_the_header() -> Result {
    let (provider, _) = memory().await?;
    let path = StreamPath::new("empty")?;
    provider
        .fork(ForkRequest {
            source: StreamPath::new("task-volume")?,
            destination: path.clone(),
            at_tail: Some(0),
            idempotency_key: None,
        })
        .await?;
    assert_eq!(provider.tail(path.clone()).await?, 0);
    let (first, second) = tokio::join!(
        TaskVolume::open(provider.clone(), path.clone()),
        TaskVolume::open(provider.clone(), path.clone()),
    );
    let first = first?.snapshot(&Query::Root, 0).await?;
    let second = second?.snapshot(&Query::Root, 0).await?;
    assert_eq!(first.cursor, second.cursor);
    assert_eq!(provider.tail(path).await?, 1);
    Ok(())
}

#[tokio::test]
async fn reused_accepted_positions_reset_cached_heads_and_resume_cursors() -> Result {
    let (original, original_volume) = memory().await?;
    let path = StreamPath::new("task-volume")?;
    let mut records = original
        .read(ReadRequest {
            path: path.clone(),
            from: 0,
            limit: 1,
        })
        .await?;
    let header = records.next().await.ok_or("missing header")??.value;
    let provider = Arc::new(Restartable(std::sync::RwLock::new(original)));
    let cached = TaskVolume::open(provider.clone(), path.clone()).await?;
    let initial = original_volume.snapshot(&Query::Root, 0).await?;
    original_volume
        .append(initial.generation, &write("GOAL.md", "lost")?)
        .await?;
    let stale = cached.snapshot(&Query::Root, 1).await?;

    let replacement = Arc::new(MemoryStream::default());
    replacement
        .append(AppendRequest {
            path: path.clone(),
            records: vec![header],
            if_tail: Some(0),
            idempotency_key: None,
        })
        .await?;
    let recovered = TaskVolume::open(replacement.clone(), path).await?;
    let initial = recovered.snapshot(&Query::Root, 0).await?;
    let replacement_file = write("GOAL.md", "replacement")?;
    recovered
        .append(initial.generation, &replacement_file)
        .await?;
    let current = recovered.snapshot(&Query::Root, 1).await?;
    assert_eq!(stale.cursor.position, current.cursor.position);
    assert_ne!(stale.cursor.commit, current.cursor.commit);
    *provider.0.write().map_err(|_| "restart lock poisoned")? = replacement;

    assert!(matches!(
        cached.snapshot(&Query::Root, 1).await,
        Err(Error::ResetRequired)
    ));
    assert!(matches!(
        cached.heads(&Query::Root, None, 1).await,
        Err(Error::ResetRequired)
    ));
    assert!(matches!(
        cached
            .append(stale.generation, &write("GOAL.md", "stale")?)
            .await,
        Err(Error::ResetRequired)
    ));
    let mut follows = Vec::new();
    for cursor in [None, Some(stale.cursor)] {
        let observation = cached.observe(Query::Root, cursor, 1).await?;
        assert!(observation.reset);
        let snapshot = observation.snapshot.ok_or("missing replacement snapshot")?;
        assert_eq!(snapshot.files, vec![replacement_file.clone()]);
        assert_eq!(snapshot.cursor, current.cursor);
        follows.push(observation.changes);
    }
    let next = write("GOAL.md", "next")?;
    recovered.append(current.generation, &next).await?;
    for mut follow in follows {
        assert_eq!(
            follow.next().await.ok_or("missing follow")??.file,
            Some(next.clone())
        );
    }
    Ok(())
}

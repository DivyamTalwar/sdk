use super::*;
use acyclic_stream::{
    AppendOutcome, AppendRequest, ChildStream, ChildrenRequest, CommitId, CommitOutcome,
    CommitRequest, CommittedEnvelope, ForkReceipt, ForkRequest, IdempotencyKey,
    IdempotencyObservation, RecordStream, StreamBounds, StreamError,
};

type StreamResult<T> = std::result::Result<T, StreamError>;

struct AckGate {
    inner: MemoryStream,
    accepted: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[async_trait::async_trait]
impl StreamProvider for AckGate {
    async fn inspect_idempotency(
        &self,
        key: IdempotencyKey,
    ) -> StreamResult<Option<IdempotencyObservation>> {
        self.inner.inspect_idempotency(key).await
    }
    async fn tail(&self, path: StreamPath) -> StreamResult<u64> {
        self.inner.tail(path).await
    }
    async fn bounds(&self, path: StreamPath) -> StreamResult<StreamBounds> {
        self.inner.bounds(path).await
    }
    async fn append(&self, request: AppendRequest) -> StreamResult<AppendOutcome> {
        self.inner.append(request).await
    }
    async fn fork(&self, request: ForkRequest) -> StreamResult<ForkReceipt> {
        self.inner.fork(request).await
    }
    async fn read(&self, request: ReadRequest) -> StreamResult<RecordStream> {
        self.inner.read(request).await
    }
    async fn follow(&self, path: StreamPath, from: u64) -> StreamResult<RecordStream> {
        self.inner.follow(path, from).await
    }
    async fn children(&self, request: ChildrenRequest) -> StreamResult<ChildStream> {
        self.inner.children(request).await
    }
    async fn read_commit(&self, id: CommitId) -> StreamResult<CommittedEnvelope> {
        self.inner.read_commit(id).await
    }
    async fn commit(&self, request: CommitRequest) -> StreamResult<CommitOutcome> {
        let outcome = self.inner.commit(request).await?;
        let signal = self
            .accepted
            .lock()
            .map_err(|_| StreamError::Unavailable)?
            .take();
        if let Some(signal) = signal {
            signal.send(()).map_err(|_| StreamError::Unavailable)?;
        }
        let release = self.release.lock().await.take();
        if let Some(release) = release {
            release.await.map_err(|_| StreamError::Unavailable)?;
        }
        Ok(outcome)
    }
}

#[tokio::test]
async fn materialized_heads_do_not_wait_for_a_pending_append_acknowledgement() -> Result {
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let provider = Arc::new(AckGate {
        inner: MemoryStream::default(),
        accepted: std::sync::Mutex::new(Some(accepted_tx)),
        release: tokio::sync::Mutex::new(Some(release_rx)),
    });
    let volume = Arc::new(TaskVolume::open(provider, StreamPath::new("task-events")?).await?);
    let initial = volume.snapshot(&Query::Root, 0).await?;
    let mut follow = volume.follow(Query::Root, initial.cursor).await?;
    let appending = tokio::spawn({
        let volume = volume.clone();
        async move {
            volume
                .append(
                    initial.generation,
                    &Write::new(FileKey::Goal, OwnedBytes::from_static(b"active")),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), accepted_rx).await??;
    let head =
        tokio::time::timeout(Duration::from_secs(1), volume.heads(&Query::Root, None, 1)).await??;
    assert_eq!(
        head.files
            .first()
            .ok_or("missing accepted head")?
            .segments
            .concat(),
        b"active"
    );
    assert!(!appending.is_finished());
    let change = follow
        .next()
        .now_or_never()
        .ok_or("follow waited for append acknowledgement")?
        .ok_or("follow ended")??;
    assert_eq!(change.file, Some(write("GOAL.md", "active")?));
    release_tx
        .send(())
        .map_err(|_| "append stopped before acknowledgement")?;
    assert_eq!(appending.await??, head.generation);
    Ok(())
}

//! Request files survive reconnects and checkpoint failures in publication order.
#![cfg(not(target_arch = "wasm32"))]
use acyclic_harness::{
    client::{FileClient, FileClientDurability, FileClientLimits},
    task_volume::{FileKey, Query, TaskVolume, Write},
};
use acyclic_stream::*;
use futures::StreamExt as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[path = "../../harness-fuzz/support/file_client.rs"]
mod parser;

struct Provider {
    inner: MemoryStream,
    offline: AtomicBool,
    commits: AtomicUsize,
    fail_commit: AtomicUsize,
}
impl Default for Provider {
    fn default() -> Self {
        Self {
            inner: MemoryStream::default(),
            offline: AtomicBool::new(false),
            commits: AtomicUsize::new(0),
            fail_commit: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl StreamProvider for Provider {
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
        let tail = self.inner.tail(path).await?;
        Ok(StreamWatermarks {
            accepted: tail,
            durable: tail,
        })
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
        self.inner.follow_checked(path, from, preceding).await
    }
    async fn children(&self, request: ChildrenRequest) -> Result<ChildStream, StreamError> {
        self.inner.children(request).await
    }
    async fn commit(&self, request: CommitRequest) -> Result<CommitOutcome, StreamError> {
        let number = self.commits.fetch_add(1, Ordering::SeqCst) + 1;
        if self.offline.load(Ordering::SeqCst) || self.fail_commit.load(Ordering::SeqCst) == number
        {
            return Err(StreamError::Unavailable);
        }
        self.inner.commit(request).await
    }
    async fn read_commit(&self, id: CommitId) -> Result<CommittedEnvelope, StreamError> {
        self.inner.read_commit(id).await
    }
}

const LIMITS: FileClientLimits = FileClientLimits {
    commands: 64,
    bytes: 4096,
    durability: FileClientDurability::Durable,
};
fn path(value: &str) -> StreamPath {
    StreamPath::new(value).unwrap()
}
fn request(id: u8) -> Write {
    Write::new(
        FileKey::parse(&format!(".tasks/task/requests/op{id}.json")).unwrap(),
        vec![id],
    )
}
async fn volume(provider: Arc<Provider>, name: &str) -> Arc<TaskVolume<Provider>> {
    Arc::new(TaskVolume::open(provider, path(name)).await.unwrap())
}

#[tokio::test]
async fn failed_admission_blocks_later_requests_across_reopen() {
    let local = Arc::new(Provider::default());
    let remote = Arc::new(Provider::default());
    let outbox = volume(local, "client/files").await;
    let target = volume(remote.clone(), "agent/files").await;
    let cut = target.cut(&Query::Tasks).await.unwrap();
    let mut client = FileClient::open(outbox.clone(), target.clone(), LIMITS)
        .await
        .unwrap();
    client.enqueue(request(9)).await.unwrap();
    remote.offline.store(true, Ordering::SeqCst);
    assert!(client.flush().await.is_err());
    client.enqueue(request(1)).await.unwrap();
    assert!(client.flush().await.is_err());
    assert_eq!(client.pending(), 2);
    assert!(
        target
            .snapshot(&Query::Tasks, 2)
            .await
            .unwrap()
            .files
            .is_empty()
    );
    drop(client);
    let mut client = FileClient::open(outbox, target.clone(), LIMITS)
        .await
        .unwrap();
    assert_eq!(client.pending(), 2);
    remote.offline.store(false, Ordering::SeqCst);
    assert_eq!(client.flush().await.unwrap(), 2);
    let mut changes = target.follow(Query::Tasks, cut).await.unwrap();
    for id in [9, 1] {
        assert_eq!(
            changes
                .next()
                .await
                .unwrap()
                .unwrap()
                .file
                .unwrap()
                .bytes
                .as_ref(),
            &[id]
        );
    }
    assert_eq!(client.flush().await.unwrap(), 0);
}

#[tokio::test]
async fn checkpoint_failure_retains_an_already_published_request() {
    let local = Arc::new(Provider::default());
    let outbox = volume(local.clone(), "client/files").await;
    let target = volume(Arc::new(Provider::default()), "agent/files").await;
    let mut client = FileClient::open(outbox.clone(), target.clone(), LIMITS)
        .await
        .unwrap();
    client.enqueue(request(1)).await.unwrap();
    local
        .fail_commit
        .store(local.commits.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
    assert!(client.flush().await.is_err());
    assert_eq!(client.pending(), 1);
    let accepted = target.head().await.unwrap();
    drop(client);
    let mut client = FileClient::open(outbox.clone(), target.clone(), LIMITS)
        .await
        .unwrap();
    assert_eq!(client.pending(), 1);
    assert_eq!(client.flush().await.unwrap(), 1);
    assert_eq!(
        target.head().await.unwrap(),
        accepted,
        "retry must not append another request"
    );
    drop(client);
    assert_eq!(
        FileClient::open(outbox, target, LIMITS)
            .await
            .unwrap()
            .pending(),
        0
    );
}

#[tokio::test]
async fn quotas_and_target_identity_fail_before_submission() {
    let outbox = volume(Arc::new(Provider::default()), "client/files").await;
    let target = volume(Arc::new(Provider::default()), "agent/files").await;
    let mut client = FileClient::open(
        outbox.clone(),
        target.clone(),
        FileClientLimits {
            commands: 1,
            bytes: 1,
            durability: FileClientDurability::Durable,
        },
    )
    .await
    .unwrap();
    client.enqueue(request(1)).await.unwrap();
    client.enqueue(request(1)).await.unwrap();
    let before = outbox.head().await.unwrap();
    assert!(client.enqueue(request(2)).await.is_err());
    assert!(
        client
            .enqueue(Write::new(FileKey::Goal, b"goal".to_vec()))
            .await
            .is_err()
    );
    assert_eq!(outbox.head().await.unwrap(), before);
    drop(client);
    let replacement = volume(Arc::new(Provider::default()), "agent/files").await;
    assert!(FileClient::open(outbox, replacement, LIMITS).await.is_err());
    assert!(
        target
            .snapshot(&Query::Tasks, 1)
            .await
            .unwrap()
            .files
            .is_empty()
    );
}

fn conserved(enqueued: usize, accepted: usize, pending: usize) -> bool {
    enqueued == accepted + pending
}

#[tokio::test]
async fn negative_control_acknowledging_before_target_publication_is_detected() {
    let outbox = volume(Arc::new(Provider::default()), "client/files").await;
    let target = volume(Arc::new(Provider::default()), "agent/files").await;
    let mut client = FileClient::open(outbox.clone(), target.clone(), LIMITS)
        .await
        .unwrap();
    client.enqueue(request(1)).await.unwrap();
    assert!(conserved(1, 0, client.pending()));
    drop(client);
    let key = FileKey::parse("workspace/client/outbox.json").unwrap();
    let state = outbox.snapshot(&Query::File(key.clone()), 1).await.unwrap();
    let mut checkpoint: serde_json::Value = serde_json::from_slice(&state.files[0].bytes).unwrap();
    checkpoint["cursor"] = serde_json::to_value(outbox.cut(&Query::Tasks).await.unwrap()).unwrap();
    outbox
        .append(
            state.generation,
            &Write::new(key, serde_json::to_vec(&checkpoint).unwrap()),
        )
        .await
        .unwrap();
    let broken = FileClient::open(outbox, target, LIMITS).await.unwrap();
    assert!(!conserved(1, 0, broken.pending()));
}

proptest::proptest! {
    #[test]
    fn checkpoint_codec_and_recovery_preserve_request_identity(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256)) {
        parser::exercise(&bytes);
    }

    // Each first-seen immutable request is delivered once in enqueue order after
    // reopen; repeated identities neither reorder nor duplicate publication.
    #[test]
    fn recovery_preserves_order_and_idempotence(ids in proptest::collection::vec(0u8..16, 0..32)) {
        futures::executor::block_on(async {
            let outbox = volume(Arc::new(Provider::default()), "client/files").await;
            let target = volume(Arc::new(Provider::default()), "agent/files").await;
            let cut = target.cut(&Query::Tasks).await.unwrap();
            let mut client = FileClient::open(outbox.clone(), target.clone(), LIMITS).await.unwrap();
            let mut expected = Vec::new();
            for id in ids { client.enqueue(request(id)).await.unwrap(); if !expected.contains(&id) { expected.push(id); } }
            assert!(conserved(expected.len(), 0, client.pending()));
            drop(client);
            let mut client = FileClient::open(outbox, target.clone(), LIMITS).await.unwrap();
            assert_eq!(client.flush().await.unwrap(), expected.len());
            let mut changes = target.follow(Query::Tasks, cut).await.unwrap();
            for id in expected { assert_eq!(changes.next().await.unwrap().unwrap().file.unwrap().bytes.as_ref(), &[id]); }
            assert_eq!(client.pending(), 0);
        });
    }
}

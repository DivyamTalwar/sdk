use super::*;
use crate::{AppendRequest, MemoryStream, OwnedBytes};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tonic::{Response as Reply, Status, Streaming, transport::Endpoint};
use wire::stream_session_service_server::{StreamSessionService, StreamSessionServiceServer};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
#[derive(Default)]
struct Counts {
    connections: AtomicUsize,
    sessions: AtomicUsize,
    follows: AtomicUsize,
    actors: AtomicUsize,
    acknowledgements: AtomicUsize,
    changed: tokio::sync::Notify,
    // Fixture-only lock: never nested, and never held across an await.
    resumes: Mutex<Vec<(u64, u64, u64)>>,
}
struct Host<P> {
    provider: Arc<P>,
    counts: Arc<Counts>,
    node: String,
    controls: tokio::sync::broadcast::Sender<Result<wire::SessionFrame, Status>>,
}
#[tonic::async_trait]
impl<P: StreamProvider> StreamSessionService for Host<P> {
    type SessionStream = BoxStream<'static, Result<wire::SessionFrame, Status>>;
    async fn session(
        &self,
        request: Request<Streaming<wire::SessionFrame>>,
    ) -> Result<Reply<Self::SessionStream>, Status> {
        assert_eq!(
            request
                .metadata()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer fixture")
        );
        self.counts.sessions.fetch_add(1, Ordering::Relaxed);
        let counts = Arc::clone(&self.counts);
        let input = request
            .into_inner()
            .map(move |input| {
                if let Ok(wire::SessionFrame {
                    body: Some(Body::Interest(interest)),
                    ..
                }) = &input
                {
                    for subscription in &interest.upsert {
                        match subscription.kind {
                            Some(Kind::Follow(_)) => {
                                counts.follows.fetch_add(1, Ordering::Relaxed);
                                if let (Some(channel), Some(resume)) =
                                    (subscription.channel, subscription.resume.as_ref())
                                {
                                    counts
                                        .resumes
                                        .lock()
                                        .map_err(|_| StreamError::Unavailable)?
                                        .push((
                                            channel.channel_id,
                                            channel.generation,
                                            resume.next,
                                        ));
                                }
                                counts.changed.notify_one();
                            }
                            Some(Kind::ActorSocket(_) | Kind::ActorCall(_)) => {
                                counts.actors.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                }
                if matches!(
                    &input,
                    Ok(wire::SessionFrame {
                        body: Some(Body::Ack(_)),
                        ..
                    })
                ) {
                    counts.acknowledgements.fetch_add(1, Ordering::Relaxed);
                }
                input.map_err(|_| StreamError::Unavailable)
            })
            .boxed();
        let output = serve(
            Arc::clone(&self.provider),
            self.node.clone(),
            Limits::default(),
            input,
        )
        .map_err(|error| Status::invalid_argument(error.code()))?;
        let controls = stream::unfold(self.controls.subscribe(), |mut receiver| async move {
            receiver.recv().await.ok().map(|frame| (frame, receiver))
        });
        Ok(Reply::new(
            stream::select(
                output.map(|item| item.map_err(|error| Status::failed_precondition(error.code()))),
                controls,
            )
            .boxed(),
        ))
    }
}
struct Fixture {
    controls: tokio::sync::broadcast::Sender<Result<wire::SessionFrame, Status>>,
    channel: TransportChannel,
    counts: Arc<Counts>,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}
impl Fixture {
    async fn open<P: StreamProvider>(
        provider: Arc<P>,
        node: &str,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let counts = Arc::new(Counts::default());
        let accepted = Arc::clone(&counts);
        let incoming = stream::try_unfold(listener, move |listener| {
            let counts = Arc::clone(&accepted);
            async move {
                let (socket, _) = listener.accept().await?;
                socket.set_nodelay(true)?;
                counts.connections.fetch_add(1, Ordering::Relaxed);
                Ok::<_, std::io::Error>(Some((socket, listener)))
            }
        });
        let (controls, _) = tokio::sync::broadcast::channel(16);
        let unary = grpc::Service::new(Arc::clone(&provider));
        let host = Host {
            provider,
            counts: Arc::clone(&counts),
            node: node.into(),
            controls: controls.clone(),
        };
        let server = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(wire::stream_service_server::StreamServiceServer::new(unary))
                .add_service(StreamSessionServiceServer::new(host))
                .serve_with_incoming(incoming),
        );
        let channel = Endpoint::from_shared(endpoint)?
            .tcp_nodelay(true)
            .connect()
            .await?;
        Ok(Self {
            controls,
            channel,
            counts,
            server,
        })
    }
    fn client(&self) -> Result<grpc::Client, Box<dyn std::error::Error + Send + Sync>> {
        Ok(grpc::Client::from_channel(
            self.channel.clone(),
            "Bearer fixture".parse()?,
        ))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn append(provider: &MemoryStream, path: &str, count: usize) -> Result<(), StreamError> {
    provider
        .append(AppendRequest {
            path: StreamPath::new(path)?,
            records: vec![OwnedBytes::from_static(b"delta"); count],
            if_tail: None,
            idempotency_key: None,
        })
        .await?;
    Ok(())
}
async fn next(
    records: &mut RecordStream,
) -> Result<crate::Record, Box<dyn std::error::Error + Send + Sync>> {
    Ok(tokio::time::timeout(Duration::from_secs(1), records.next())
        .await?
        .ok_or("follow ended")??)
}
#[tokio::test]
async fn public_follow_shares_one_session_and_slow_channels_do_not_block_others() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    append(&provider, "slow", 1).await?;
    append(&provider, "fast", 1).await?;
    let fixture = Fixture::open(Arc::clone(&provider), "node").await?;
    let client = fixture.client()?;
    let mut slow = client.follow(StreamPath::new("slow")?, 1).await?;
    let mut fast = client.clone().follow(StreamPath::new("fast")?, 1).await?;
    append(&provider, "slow", 20).await?;
    append(&provider, "fast", 20).await?;
    for sequence in 1..=20 {
        assert_eq!(next(&mut fast).await?.sequence, sequence);
    }
    for sequence in 1..=20 {
        assert_eq!(next(&mut slow).await?.sequence, sequence);
    }
    drop(slow);
    append(&provider, "fast", 1).await?;
    assert_eq!(next(&mut fast).await?.sequence, 21);
    assert_eq!(fixture.counts.connections.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.counts.follows.load(Ordering::Relaxed), 2);
    assert_eq!(fixture.counts.actors.load(Ordering::Relaxed), 0);
    Ok(())
}
#[tokio::test]
async fn refused_channel_does_not_close_the_shared_session() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    append(&provider, "fast", 1).await?;
    let fixture = Fixture::open(Arc::clone(&provider), "node").await?;
    let client = fixture.client()?;
    let mut fast = client.follow(StreamPath::new("fast")?, 1).await?;
    let mut refused = client.follow(StreamPath::new("missing")?, 0).await?;
    assert_eq!(refused.next().await, Some(Err(StreamError::NotFound)));
    assert!(refused.next().await.is_none());
    append(&provider, "fast", 1).await?;
    assert_eq!(next(&mut fast).await?.sequence, 1);
    assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 1);
    Ok(())
}
#[tokio::test]
async fn node_subscription_fan_in_reuses_the_same_client_session_protocol() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    for index in 0..16 {
        append(&provider, &format!("agents/{index}"), 1).await?;
    }
    let owner = Fixture::open(Arc::clone(&provider), "owner").await?;
    let relay = Fixture::open(Arc::new(owner.client()?), "relay").await?;
    let client = relay.client()?;
    let mut follows = Vec::new();
    for index in 0..16 {
        follows.push(
            client
                .follow(StreamPath::new(format!("agents/{index}"))?, 1)
                .await?,
        );
    }
    for (index, follow) in follows.iter_mut().enumerate() {
        append(&provider, &format!("agents/{index}"), 1).await?;
        assert_eq!(next(follow).await?.sequence, 1);
    }
    for fixture in [&owner, &relay] {
        assert_eq!(fixture.counts.connections.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.counts.follows.load(Ordering::Relaxed), 16);
        assert_eq!(fixture.counts.actors.load(Ordering::Relaxed), 0);
    }
    Ok(())
}

#[tokio::test]
async fn moved_resumes_applied_cursor_and_dialing_does_not_block_other_channels() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    for path in ["move", "stay"] {
        append(&provider, path, 1).await?;
    }
    let old = Fixture::open(Arc::clone(&provider), "old").await?;
    let new = Fixture::open(Arc::clone(&provider), "new").await?;
    let (release, blocked) = oneshot::channel();
    let blocked = Arc::new(Mutex::new(Some(blocked)));
    let channel = new.channel.clone();
    let dials = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&dials);
    let started = Arc::new(tokio::sync::Notify::new());
    let dialing = Arc::clone(&started);
    let connect: Connect = Arc::new(move |node| {
        assert_eq!(node.node_id, "new");
        observed.fetch_add(1, Ordering::Relaxed);
        dialing.notify_one();
        let gate = blocked.lock().ok().and_then(|mut gate| gate.take());
        let channel = channel.clone();
        Box::pin(async move {
            gate.ok_or(StreamError::Unavailable)?
                .await
                .map_err(|_| StreamError::Unavailable)?;
            Ok(channel)
        })
    });
    let client =
        Client::with_connector(old.channel.clone(), "Bearer fixture".parse()?, connect).await?;
    let mut moving = client.follow(StreamPath::new("move")?, 1).await?;
    let mut staying = client.follow(StreamPath::new("stay")?, 1).await?;
    append(&provider, "move", 4).await?;
    assert_eq!(next(&mut moving).await?.sequence, 1);
    old.controls.send(Ok(frame(
        1,
        1,
        Body::Moved(wire::SessionMoved {
            node: Some(wire::SessionNode {
                node_id: "new".into(),
                endpoint: "https://new.invalid".into(),
                placement_epoch: 1,
            }),
            resume: Some(cursor(999)),
        }),
    )))?;
    tokio::time::timeout(Duration::from_secs(1), started.notified()).await?;
    // The destination dial is blocked. Unrelated follows must still be live.
    append(&provider, "stay", 1).await?;
    assert_eq!(next(&mut staying).await?.sequence, 1);
    release.send(()).map_err(|_| "dial gate dropped")?;
    tokio::time::timeout(Duration::from_secs(1), new.counts.changed.notified()).await?;
    assert_eq!(
        new.counts
            .resumes
            .lock()
            .map_err(|_| "resumes poisoned")?
            .as_slice(),
        &[(1, 2, 2)]
    );
    append(&provider, "move", 1).await?;
    for sequence in 2..=5 {
        assert_eq!(next(&mut moving).await?.sequence, sequence);
    }
    assert_eq!(dials.load(Ordering::Relaxed), 1);
    for fixture in [&old, &new] {
        assert_eq!(fixture.counts.connections.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.counts.actors.load(Ordering::Relaxed), 0);
    }
    Ok(())
}

#[tokio::test]
async fn append_to_follow_latency() -> TestResult {
    let memory = Arc::new(MemoryStream::default());
    measure(memory.as_ref(), "memory").await?;
    let fixture = Fixture::open(Arc::new(MemoryStream::default()), "node").await?;
    measure(&fixture.client()?, "grpc_tcp").await?;
    println!(
        "sessions={} connections={}",
        fixture.counts.sessions.load(Ordering::Relaxed),
        fixture.counts.connections.load(Ordering::Relaxed)
    );
    Ok(())
}
async fn measure<P: StreamProvider>(provider: &P, label: &str) -> TestResult {
    let path = StreamPath::new("latency")?;
    provider
        .append(AppendRequest {
            path: path.clone(),
            records: vec![OwnedBytes::from_static(b"seed")],
            if_tail: None,
            idempotency_key: None,
        })
        .await?;
    let mut follow = provider.follow(path.clone(), 1).await?;
    let mut samples = Vec::new();
    for sequence in 1..=144 {
        let started = std::time::Instant::now();
        provider
            .append(AppendRequest {
                path: path.clone(),
                records: vec![OwnedBytes::from_static(b"delta")],
                if_tail: None,
                idempotency_key: None,
            })
            .await?;
        let record = next(&mut follow).await?;
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(record.sequence, sequence);
        assert_eq!(record.value.as_slice(), b"delta");
        if sequence > 16 {
            samples.push(elapsed);
        }
    }
    samples.sort_unstable();
    println!(
        "append_to_follower {label} n={} p50_ns={} p99_ns={}",
        samples.len(),
        samples.get(63).ok_or("p50 missing")?,
        samples.get(126).ok_or("p99 missing")?
    );
    Ok(())
}

#[tokio::test]
async fn transport_reconnect_reopens_all_channels_from_consumed_cursors() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    for path in ["a", "b"] {
        append(&provider, path, 1).await?;
    }
    let fixture = Fixture::open(Arc::clone(&provider), "node").await?;
    let client = fixture.client()?;
    let mut a = client.follow(StreamPath::new("a")?, 1).await?;
    let mut b = client.follow(StreamPath::new("b")?, 1).await?;
    for path in ["a", "b"] {
        append(&provider, path, 4).await?;
    }
    assert_eq!(next(&mut a).await?.sequence, 1);
    assert_eq!(next(&mut b).await?.sequence, 1);
    fixture
        .controls
        .send(Err(Status::unavailable("disconnect")))?;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let changed = fixture.counts.changed.notified();
            if fixture
                .counts
                .resumes
                .lock()
                .map_err(|_| StreamError::Unavailable)?
                .len()
                >= 4
            {
                return Ok::<(), StreamError>(());
            }
            changed.await;
        }
    })
    .await??;
    let resumed = fixture
        .counts
        .resumes
        .lock()
        .map_err(|_| "resumes poisoned")?
        .iter()
        .copied()
        .filter(|(_, generation, _)| *generation == 2)
        .collect::<Vec<_>>();
    assert_eq!(resumed.len(), 2);
    assert!(resumed.iter().all(|(_, _, next)| *next == 2));
    for path in ["a", "b"] {
        append(&provider, path, 1).await?;
    }
    for sequence in 2..=5 {
        assert_eq!(next(&mut a).await?.sequence, sequence);
        assert_eq!(next(&mut b).await?.sequence, sequence);
    }
    assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 2);
    assert_eq!(fixture.counts.connections.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.counts.actors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[tokio::test]
async fn cancelling_a_queued_open_does_not_leave_an_idle_subscription() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    append(&provider, "a", 1).await?;
    let fixture = Fixture::open(Arc::clone(&provider), "node").await?;
    let client = Client::open(fixture.channel.clone(), "Bearer fixture".parse()?, None).await?;
    let mut cancelled = Box::pin(client.follow(StreamPath::new("a")?, 1));
    assert!(futures::poll!(cancelled.as_mut()).is_pending());
    drop(cancelled);
    let mut follow = client.follow(StreamPath::new("a")?, 1).await?;
    append(&provider, "a", 1).await?;
    assert_eq!(next(&mut follow).await?.sequence, 1);
    assert_eq!(fixture.counts.follows.load(Ordering::Relaxed), 1);
    Ok(())
}

#[tokio::test]
async fn aggregate_credit_is_replenished_beyond_the_initial_window() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    append(&provider, "a", 1).await?;
    let fixture = Fixture::open(Arc::clone(&provider), "node").await?;
    let client = fixture.client()?;
    let mut records = client.follow(StreamPath::new("a")?, 1).await?;
    append(&provider, "a", crate::MAX_ITEMS).await?;
    append(&provider, "a", 17).await?;
    for sequence in 1..=u64::try_from(crate::MAX_ITEMS + 17)? {
        assert_eq!(next(&mut records).await?.sequence, sequence);
    }
    assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 1);
    Ok(())
}

#[tokio::test]
async fn translating_follow_releases_credit_without_application_acknowledgements() -> TestResult {
    let provider = Arc::new(MemoryStream::default());
    append(&provider, "a", 1).await?;
    let fixture = Fixture::open(Arc::clone(&provider), "node").await?;
    let client = fixture.client()?.without_delivery_acknowledgements();
    let mut records = client.follow(StreamPath::new("a")?, 1).await?;
    append(&provider, "a", 17).await?;
    for sequence in 1..=17 {
        assert_eq!(next(&mut records).await?.sequence, sequence);
    }
    assert_eq!(fixture.counts.acknowledgements.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 1);
    Ok(())
}

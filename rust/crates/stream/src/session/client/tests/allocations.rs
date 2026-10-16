use super::*;
use std::{future::Future, hint::black_box, task::Poll};

// Measure only the poll that completes registration. The real public follow
// future runs unchanged; dial/registration allocations happen on earlier polls,
// and other Tokio tasks cannot execute within this synchronous measurement.
async fn completion_allocations(
    open: impl Future<Output = Result<RecordStream, StreamError>>,
) -> Result<allocation_counter::AllocationInfo, StreamError> {
    let mut open = std::pin::pin!(open);
    let (records, allocation) = future::poll_fn(|cx| {
        let mut outcome = Poll::Pending;
        let allocation = allocation_counter::measure(|| outcome = open.as_mut().poll(cx));
        match outcome {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => Poll::Ready(result.map(|records| (records, allocation))),
        }
    })
    .await?;
    drop(black_box(records));
    Ok(allocation)
}

// Restore precisely the reviewed 9f38e976e projection shape: boxed received
// stream, boxed canonical data projection, then boxed typed-record projection.
// It uses the same real registration, cursor, credit and observation as today.
async fn reviewed_follow(client: &Client, path: StreamPath) -> Result<RecordStream, StreamError> {
    let received = client
        .open_kind(
            Kind::Follow(wire::SessionFollow {
                path: path.to_string(),
                delivery: wire::SessionDelivery::All as i32,
                ..Default::default()
            }),
            1,
            true,
        )
        .await?
        .boxed();
    let data = received
        .map(|item| match item? {
            Received::Data(data) => Ok(data),
            _ => Err(StreamError::Unavailable),
        })
        .boxed();
    Ok(data
        .map(|data| match data?.item {
            Some(Item::Record(record)) => wire_codec::record(record),
            _ => Err(StreamError::Unavailable),
        })
        .boxed())
}

fn public_box_budget(allocations: u64, follows: u64) {
    assert_eq!(
        allocations, follows,
        "typed follow must allocate only the final public stream box"
    );
}

async fn paired() -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
    const FOLLOWS: u64 = 128;
    let provider = Arc::new(MemoryStream::default());
    append(&provider, "events", 1).await?;
    let fixture = Fixture::open(provider, "node").await?;
    let client = Client::open(
        fixture.channel.clone(),
        Some("Bearer fixture".parse()?),
        None,
        None,
        None,
    );
    // Keep one interest alive so both arms share one warm physical Session.
    let anchor = client.follow(StreamPath::new("events")?, 1).await?;
    allocation_counter::measure(|| {});
    let mut before = (0, 0);
    let mut after = (0, 0);
    for index in 0..FOLLOWS {
        // Alternate the order to expose a dependency on earlier arm setup.
        for legacy in if index % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let path = StreamPath::new("events")?;
            let allocation = if legacy {
                completion_allocations(reviewed_follow(&client, path)).await?
            } else {
                completion_allocations(client.follow(path, 1)).await?
            };
            let total = if legacy { &mut before } else { &mut after };
            total.0 += allocation.count_total;
            total.1 += allocation.bytes_total;
            if !legacy {
                public_box_budget(allocation.count_total, 1);
            }
        }
    }
    assert_eq!(before.0, 3 * FOLLOWS);
    public_box_budget(after.0, FOLLOWS);
    assert_eq!(
        before.1 - after.1,
        2 * FOLLOWS * std::mem::size_of::<RecordStream>() as u64
    );
    assert_eq!(fixture.counts.sessions.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.counts.connections.load(Ordering::Relaxed), 1);
    eprintln!(
        "follow_projection_allocations follows={FOLLOWS} before_count={} after_count={} before_bytes={} after_bytes={}",
        before.0, after.0, before.1, after.1
    );
    drop(anchor);
    Ok((before.0, FOLLOWS))
}

#[tokio::test]
async fn typed_follow_boxes_only_at_its_public_boundary() -> TestResult {
    tokio::time::timeout(Duration::from_secs(3), paired()).await??;
    Ok(())
}

#[tokio::test]
#[should_panic(expected = "typed follow must allocate only the final public stream box")]
async fn negative_control_rejects_the_reviewed_redundant_boxes() {
    let (before, follows) = paired().await.expect("allocation pair failed");
    public_box_budget(before, follows);
}

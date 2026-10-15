use super::*;
use wasm_bindgen_test::*;
wasm_bindgen_test_configure!(run_in_browser);

fn name() -> String {
    format!("discovery-{}", uuid::Uuid::new_v4())
}
async fn append(provider: &BrowserStream, path: &str) -> Result<AppendOutcome, StreamError> {
    provider
        .append(AppendRequest {
            path: StreamPath::new(path)?,
            records: vec![OwnedBytes::from_static(b"value")],
            if_tail: None,
            idempotency_key: None,
        })
        .await
}
fn request() -> ChildrenRequest {
    ChildrenRequest {
        parent: None,
        limit: 1,
    }
}
async fn child(children: &mut ChildStream) -> StreamPath {
    children
        .next()
        .await
        .expect("discovery ended")
        .expect("child error")
        .path
}

#[wasm_bindgen_test]
async fn discovery_never_exposes_a_capacity_rejected_write() {
    let provider = BrowserStream::open(
        &name(),
        BrowserStreamLimits {
            commands: 1,
            ..Default::default()
        },
    )
    .await
    .expect("open");
    let mut children = provider.children(request()).await.expect("subscribe");
    append(&provider, "a").await.expect("accepted");
    assert_eq!(child(&mut children).await.as_str(), "a");
    assert_eq!(append(&provider, "b").await, Err(StreamError::Capacity));
    let next = Box::pin(children.next());
    let refreshed = Box::pin(provider.tail(StreamPath::new("b").expect("path")));
    match futures::future::select(next, refreshed).await {
        futures::future::Either::Left(_) => panic!("rejected child was published"),
        futures::future::Either::Right((tail, mut next)) => {
            assert_eq!(tail, Err(StreamError::NotFound));
            assert!(futures::poll!(next.as_mut()).is_pending());
        }
    }
}

#[wasm_bindgen_test]
async fn discovery_survives_cache_replacement_after_a_rejected_command() {
    let provider = BrowserStream::open(&name(), BrowserStreamLimits::default())
        .await
        .expect("open");
    let mut children = provider.children(request()).await.expect("subscribe");
    append(&provider, "a").await.expect("append");
    assert_eq!(child(&mut children).await.as_str(), "a");
    assert_eq!(
        provider
            .fork(ForkRequest {
                source: StreamPath::new("missing").expect("path"),
                destination: StreamPath::new("rejected").expect("path"),
                at_tail: None,
                idempotency_key: None
            })
            .await,
        Err(StreamError::NotFound)
    );
    provider
        .tail(StreamPath::new("a").expect("path"))
        .await
        .expect("refresh cache");
    append(&provider, "b").await.expect("append");
    assert_eq!(child(&mut children).await.as_str(), "b");
}

#[wasm_bindgen_test]
async fn discovery_wakes_for_another_database_opener() {
    let database = name();
    let reader = BrowserStream::open(&database, BrowserStreamLimits::default())
        .await
        .expect("reader");
    let writer = BrowserStream::open(&database, BrowserStreamLimits::default())
        .await
        .expect("writer");
    let mut children = reader.children(request()).await.expect("subscribe");
    assert!(futures::poll!(children.next()).is_pending());
    let (received, written) = futures::join!(child(&mut children), append(&writer, "remote"));
    written.expect("append");
    assert_eq!(received.as_str(), "remote");
}

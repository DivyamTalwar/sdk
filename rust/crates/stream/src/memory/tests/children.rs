use super::*;

#[tokio::test]
async fn children_replays_every_page_then_pushes_new_paths_once() -> Result<(), StreamError> {
    let provider = MemoryStream::default();
    for path in ["agents/m", "agents/z", "elsewhere/a"] {
        append(&provider, path).await?;
    }
    let mut children = provider
        .children(ChildrenRequest {
            parent: Some(StreamPath::new("agents")?),
            limit: 1,
        })
        .await?;
    assert_eq!(
        children.next().await.transpose()?.map(|child| child.path),
        Some(StreamPath::new("agents/m")?)
    );
    // Creation before the replay finishes must not be lost, even before the cursor.
    append(&provider, "agents/a/nested").await?;
    assert_eq!(
        children.next().await.transpose()?.map(|child| child.path),
        Some(StreamPath::new("agents/a")?)
    );
    assert_eq!(
        children.next().await.transpose()?.map(|child| child.path),
        Some(StreamPath::new("agents/z")?)
    );
    assert!(futures::poll!(children.next()).is_pending());
    append(&provider, "agents/m").await?;
    append(&provider, "elsewhere/b").await?;
    assert!(futures::poll!(children.next()).is_pending());
    append(&provider, "agents/0").await?;
    assert_eq!(
        children.next().await.transpose()?.map(|child| child.path),
        Some(StreamPath::new("agents/0")?)
    );
    assert!(futures::poll!(children.next()).is_pending());
    Ok(())
}

#[tokio::test]
async fn children_subscribes_before_first_poll_and_discovers_forks() -> Result<(), StreamError> {
    let provider = MemoryStream::default();
    let mut roots = provider
        .children(ChildrenRequest {
            parent: None,
            limit: 2,
        })
        .await?;
    let mut children = provider
        .children(ChildrenRequest {
            parent: Some(StreamPath::new("agents")?),
            limit: 2,
        })
        .await?;
    append(&provider, "source").await?;
    provider
        .fork(ForkRequest {
            source: StreamPath::new("source")?,
            destination: StreamPath::new("agents/new")?,
            at_tail: None,
            idempotency_key: None,
        })
        .await?;
    assert_eq!(
        roots.next().await.transpose()?.map(|child| child.path),
        Some(StreamPath::new("agents")?)
    );
    assert_eq!(
        roots.next().await.transpose()?.map(|child| child.path),
        Some(StreamPath::new("source")?)
    );
    assert_eq!(
        children.next().await.transpose()?.map(|child| child.path),
        Some(StreamPath::new("agents/new")?)
    );
    assert!(futures::poll!(children.next()).is_pending());
    assert!(matches!(
        provider
            .children(ChildrenRequest {
                parent: None,
                limit: 0
            })
            .await,
        Err(StreamError::LimitExceeded)
    ));
    Ok(())
}

async fn append(provider: &MemoryStream, path: &str) -> Result<(), StreamError> {
    provider
        .append(AppendRequest {
            path: StreamPath::new(path)?,
            records: vec![Bytes::from_static(b"value")],
            if_tail: None,
            idempotency_key: None,
        })
        .await?;
    Ok(())
}

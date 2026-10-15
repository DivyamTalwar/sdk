use super::tests::{TestResult, client};
use super::*;

#[tokio::test]
async fn children_pushes_before_eof_on_the_shared_channel() -> TestResult {
    let provider = crate::MemoryStream::default();
    let transport = client(provider.clone())?;
    let mut children = transport
        .children(ChildrenRequest {
            parent: Some(StreamPath::new("agents")?),
            limit: 1,
        })
        .await?;
    for name in ["first", "second"] {
        let path = StreamPath::new(format!("agents/{name}"))?;
        provider
            .append(AppendRequest {
                path: path.clone(),
                records: vec![Bytes::from_static(b"status").into()],
                if_tail: None,
                idempotency_key: None,
            })
            .await?;
        let child = tokio::time::timeout(Duration::from_secs(1), children.next())
            .await?
            .ok_or("children ended")??;
        assert_eq!(child.path, path);
        // The long-lived subscription must leave the channel usable for other calls.
        assert_eq!(transport.tail(path).await?, 1);
    }
    assert!(futures::poll!(children.next()).is_pending());
    drop(children);
    assert_eq!(transport.tail(StreamPath::new("agents/first")?).await?, 1);
    Ok(())
}

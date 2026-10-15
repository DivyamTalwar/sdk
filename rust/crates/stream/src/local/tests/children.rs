use super::*;

#[tokio::test]
async fn local_children_releases_visibility_while_waiting() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let provider = LocalStream::open(directory.path(), LocalStreamLimits::default()).await?;
    let mut children = provider
        .children(ChildrenRequest {
            parent: Some(StreamPath::new("agents")?),
            limit: 1,
        })
        .await?;
    for name in ["first", "second"] {
        assert!(futures::poll!(children.next()).is_pending());
        let path = StreamPath::new(format!("agents/{name}"))?;
        provider
            .append(AppendRequest {
                path: path.clone(),
                records: vec![Bytes::from_static(b"status")],
                if_tail: None,
                idempotency_key: None,
            })
            .await?;
        let child = tokio::time::timeout(std::time::Duration::from_secs(1), children.next())
            .await?
            .ok_or("children ended")??;
        assert_eq!(child.path, path);
    }
    drop(children);
    drop(provider);
    let reopened = LocalStream::open(directory.path(), LocalStreamLimits::default()).await?;
    assert_eq!(
        reopened
            .children_page(ChildrenPageRequest {
                parent: Some(StreamPath::new("agents")?),
                limit: 1,
                after: None,
                hierarchy_version: None,
            })
            .await?
            .children
            .len(),
        1
    );
    Ok(())
}

//! Filesystem publications drive the actual client/session orchestration.

use acyclic_harness::{
    Error, Result,
    client::{InterestTier, SwarmChange, SwarmClientLimits},
    task_volume::{Channel, FileKey, Query, TaskVolume, Write},
};
use acyclic_stream::{MemoryStream, wire};
use futures::{FutureExt as _, SinkExt as _, StreamExt as _, channel::mpsc};
use std::{cell::RefCell, rc::Rc, sync::Arc};

use super::path;

async fn put(volume: &TaskVolume<MemoryStream>, path: &str, text: &str) -> Result<bool> {
    let cut = volume
        .snapshot(&Query::Inputs, 10)
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;
    let write = Write::new(
        FileKey::parse(path).map_err(|e| Error::Invalid(e.to_string()))?,
        text.as_bytes().to_vec(),
    );
    let after = volume
        .append(cut.generation, &write)
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;
    Ok(after.position != cut.generation.position)
}

fn child(value: &str) -> wire::SessionFrame {
    wire::SessionFrame {
        channel_id: 1,
        generation: 1,
        body: Some(wire::session_frame::Body::Data(wire::SessionData {
            after: Some(wire::SessionCursor {
                next: 1,
                ..Default::default()
            }),
            accepted: 1,
            durable: 1,
            item: Some(wire::session_data::Item::Child(wire::Child {
                path: value.into(),
            })),
            ..Default::default()
        })),
    }
}

#[tokio::test]
async fn file_writes_change_session_interests_without_losing_the_snapshot_cut() -> Result<()> {
    let provider = Arc::new(MemoryStream::default());
    let config = TaskVolume::open(provider.clone(), path("client/files"))
        .await
        .unwrap();
    put(&config, "workspace/layout.json", "{}").await?;
    put(&config, "workspace/open-views.json", "{}").await?;
    let volume = Arc::new(
        TaskVolume::open(provider.clone(), path("swarm/a/files"))
            .await
            .map_err(|e| Error::Storage(e.to_string()))?,
    );
    let initial = volume.snapshot(&Query::Root, 10).await.unwrap();
    let mut chunk = Write::new(
        FileKey::parse(".tasks/t/turns/1/chunks/1").unwrap(),
        b"before".to_vec(),
    );
    chunk.channel = Some(Channel::Text);
    volume.append(initial.generation, &chunk).await.unwrap();

    let mut client = super::client(
        SwarmClientLimits {
            agents: 10,
            selected: 2,
            layout_bytes: 1024,
            config_files: 10,
            snapshot_files: 10,
        },
    )?;
    let views = client.views();
    let (session, mut sent) = mpsc::channel::<wire::SessionInterest>(4);
    let (mut frames, incoming) = mpsc::channel::<Result<wire::SessionFrame>>(2);
    let (layouts, mut rendered) = mpsc::unbounded();
    let (data_seen, mut observed) = mpsc::unbounded();
    let snapshots = Rc::new(RefCell::new(Vec::new()));
    let delivered = Rc::new(RefCell::new(Vec::new()));
    let discovery = MemoryStream::default();
    let consumer = client.follow(
        &discovery,
        &config,
        session.sink_map_err(|e| Error::Storage(e.to_string())),
        incoming,
        |agent, maximum| {
            assert_eq!(agent, path("swarm/a"));
            let volume = volume.clone();
            async move {
                let snapshot = volume.heads(&Query::Root, None, maximum).await?;
                // Publish between acquisition at C and subscription from C.
                let mut next = Write::new(FileKey::parse(".tasks/t/turns/1/chunks/2").unwrap(), b"after".to_vec());
                next.channel = Some(Channel::Text);
                volume.append(volume.head().await?, &next).await?;
                Ok(snapshot)
            }
        },
        |update| {
            match update {
                SwarmChange::Layout => layouts.unbounded_send(views.read(|v| v.layout().clone()).unwrap()).unwrap(),
                SwarmChange::Data { overview, data, .. } => {
                    delivered.borrow_mut().push((overview, data));
                    data_seen.unbounded_send(()).unwrap();
                }
                SwarmChange::Snapshot { agent, cursor } => {
                    let body = views.read(|v| v.files(&agent).next().unwrap().segments[0].to_vec()).unwrap();
                    snapshots.borrow_mut().push((cursor, body));
                }
                SwarmChange::Watermark { .. } => panic!("unexpected watermark"),
            }
            Ok(())
        },
    );
    let writer = async {
        let discovery = sent.next().await.unwrap();
        assert_eq!(discovery.revision, 1);
        assert!(matches!(
            discovery.upsert[0].kind,
            Some(wire::session_subscription::Kind::Children(_))
        ));
        frames.send(Ok(child("swarm/unseen"))).await.unwrap();
        let overview = sent.next().await.unwrap();
        assert_eq!(overview.revision, 2);
        let Some(wire::session_subscription::Kind::Follow(status)) = &overview.upsert[0].kind
        else {
            panic!("status follow")
        };
        assert_eq!(status.path, "swarm/unseen/status");
        assert_eq!(status.delivery, wire::SessionDelivery::Latest as i32);
        let attention = wire::SessionData {
            after: Some(wire::SessionCursor {
                next: 42,
                ..Default::default()
            }),
            accepted: 42,
            durable: 41,
            item: Some(wire::session_data::Item::LatestValue(
                wire::SessionLatestValue {
                    key: b"approval".to_vec().into(),
                    value: b"waiting".to_vec().into(),
                    attention: true,
                },
            )),
            ..Default::default()
        };
        frames
            .send(Ok(wire::SessionFrame {
                channel_id: overview.upsert[0].channel.as_ref().unwrap().channel_id,
                // Platform can move a follow without changing its logical channel.
                generation: 2,
                body: Some(wire::session_frame::Body::Data(attention.clone())),
            }))
            .await
            .unwrap();
        observed.next().await.unwrap();
        assert_eq!(delivered.borrow()[0], (true, attention));
        assert_eq!(rendered.next().await.unwrap().visible().count(), 0);

        put(
            &config,
            "workspace/layout.json",
            r#"{"prefetch":["swarm/a"]}"#,
        )
        .await?;
        let selected = sent.next().await.unwrap();
        assert_eq!(selected.revision, 3);
        let content = selected.upsert.iter().find(|sub| matches!(&sub.kind,
            Some(wire::session_subscription::Kind::Follow(follow)) if follow.delivery == wire::SessionDelivery::All as i32)).unwrap();
        let snapshot_cut = snapshots.borrow()[0].0.position;
        assert_eq!(content.resume.as_ref().unwrap().next, snapshot_cut);
        assert_eq!(
            content.resume.as_ref().unwrap().opaque.as_ref(),
            snapshots.borrow()[0].0.commit
        );
        assert_eq!(
            snapshots.borrow()[0].1.as_slice(),
            b"before"
        );
        assert_eq!(
            rendered.next().await.unwrap().tier(&path("swarm/a")),
            Some(InterestTier::Prefetch)
        );
        let Some(wire::session_subscription::Kind::Follow(follow)) = &content.kind else {
            panic!("follow")
        };
        let mut tail = acyclic_stream::wire_codec::session::follow_records(
            provider.as_ref(),
            follow,
            content.resume.as_ref().unwrap(),
        )
        .await
        .unwrap();
        let record = tail.next().await.unwrap().unwrap();
        assert_eq!(record.sequence, snapshot_cut);
        frames
            .send(Ok(wire::SessionFrame {
                channel_id: content.channel.as_ref().unwrap().channel_id,
                generation: 1,
                body: Some(wire::session_frame::Body::Data(wire::SessionData {
                    after: Some(
                        acyclic_stream::wire_codec::session::record_cursor(
                            snapshot_cut + 1,
                            record.commit_id,
                        )
                        .unwrap(),
                    ),
                    accepted: snapshot_cut + 1,
                    durable: snapshot_cut + 1,
                    item: Some(wire::session_data::Item::Record(
                        acyclic_stream::wire_codec::record_wire(record),
                    )),
                    ..Default::default()
                })),
            }))
            .await
            .unwrap();
        observed.next().await.unwrap();

        put(
            &config,
            "workspace/open-views.json",
            r#"{"in_view":["swarm/a"]}"#,
        )
        .await?;
        assert_eq!(
            rendered.next().await.unwrap().tier(&path("swarm/a")),
            Some(InterestTier::InView)
        );
        assert!(
            sent.next().now_or_never().is_none(),
            "promotion must retain its follow"
        );
        assert_eq!(snapshots.borrow().len(), 1);

        put(&config, "workspace/layout.json", "{}").await?;
        assert_eq!(
            rendered.next().await.unwrap().tier(&path("swarm/a")),
            Some(InterestTier::InView)
        );
        put(&config, "workspace/open-views.json", "{}").await?;
        let removed = sent.next().await.unwrap();
        assert_eq!(
            removed.revision, 4,
            "render-only promotion sends no session revision"
        );
        assert_eq!(removed.remove, vec![content.channel.clone().unwrap()]);
        assert!(
            removed.upsert.is_empty(),
            "status and discovery remain subscribed"
        );
        assert_eq!(
            rendered.next().await.unwrap().tier(&path("swarm/a")),
            Some(InterestTier::Overview)
        );
        frames
            .send(Err(Error::Unsupported("end scenario".into())))
            .await
            .unwrap();
        Ok::<_, Error>(())
    };
    let (outcome, result) = futures::join!(consumer, writer);
    result?;
    assert!(matches!(outcome, Err(Error::Unsupported(_))));
    assert_eq!(delivered.borrow().len(), 2);
    assert!(!delivered.borrow()[1].0);
    Ok(())
}

fn check_layout(value: &acyclic_harness::client::ClientInterests, visible: &[u8], nearby: &[u8]) {
    assert_eq!(
        super::ids(value.visible()),
        visible.iter().copied().collect()
    );
    assert_eq!(
        super::ids(value.full_fidelity()),
        visible.iter().chain(nearby).copied().collect()
    );
}

async fn check_snapshot_follow(
    old_visible: Vec<u8>,
    old_nearby: Vec<u8>,
    new_visible: Vec<u8>,
    new_nearby: Vec<u8>,
) {
    use acyclic_harness::client::InterestDocument;
    let provider = Arc::new(MemoryStream::default());
    let config = TaskVolume::open(provider.clone(), path("client/files"))
        .await
        .unwrap();
    let mut agents = std::collections::BTreeMap::new();
    for id in 0..4 {
        agents.insert(
            path(&format!("swarm/{id}")),
            TaskVolume::open(provider.clone(), path(&format!("swarm/{id}/files")))
                .await
                .unwrap(),
        );
    }
    let document = |visible: &[u8], nearby: &[u8]| {
        serde_json::to_string(&InterestDocument {
            in_view: super::selected(visible).collect(),
            prefetch: super::selected(nearby).collect(),
        })
        .unwrap()
    };
    // Earlier layout must be superseded by the snapshot at C.
    put(&config, "workspace/layout.json", &document(&[], &[3]))
        .await
        .unwrap();
    put(
        &config,
        "workspace/layout.json",
        &document(&[], &old_nearby),
    )
    .await
    .unwrap();
    put(
        &config,
        "workspace/open-views.json",
        &document(&old_visible, &[]),
    )
    .await
    .unwrap();
    let mut client = super::client(
        SwarmClientLimits {
            agents: 4,
            selected: 4,
            layout_bytes: 4096,
            config_files: 10,
            snapshot_files: 10,
        },
    )
    .unwrap();
    let views = client.views();
    let (stop, stopped) = futures::channel::oneshot::channel();
    let incoming = futures::stream::once(async {
        stopped.await.unwrap();
        Err(Error::Unsupported("end scenario".into()))
    });
    let (layouts, mut rendered) = mpsc::unbounded();
    let sink = futures::sink::drain().sink_map_err(|never| match never {});
    let discovery = MemoryStream::default();
    let consumer = client.follow(
        &discovery,
        &config,
        sink,
        incoming,
        |agent, maximum| agents[&agent].heads(&Query::Root, None, maximum),
        |update| {
            if let SwarmChange::Layout = update {
                layouts.unbounded_send(views.read(|v| v.layout().clone()).unwrap()).unwrap();
            }
            Ok(())
        },
    );
    let writer = async {
        let mut current = rendered.next().await.unwrap();
        check_layout(&current, &old_visible, &old_nearby);
        if put(
            &config,
            "workspace/layout.json",
            &document(&[], &new_nearby),
        )
        .await
        .unwrap()
        {
            current = rendered.next().await.unwrap();
        }
        check_layout(&current, &old_visible, &new_nearby);
        if put(
            &config,
            "workspace/open-views.json",
            &document(&new_visible, &[]),
        )
        .await
        .unwrap()
        {
            current = rendered.next().await.unwrap();
        }
        check_layout(&current, &new_visible, &new_nearby);
        stop.send(()).unwrap();
    };
    let (outcome, ()) = futures::join!(consumer, writer);
    assert!(matches!(outcome, Err(Error::Unsupported(_))));
}

proptest::proptest! {
    // Property: a snapshot at C plus each accepted file change after C equals
    // folding the latest layout/open-view files from the complete write history.
    #[test]
    fn file_snapshot_plus_follow_equals_the_complete_fold(
        old_visible in proptest::collection::vec(0u8..4, 0..8),
        old_nearby in proptest::collection::vec(0u8..4, 0..8),
        new_visible in proptest::collection::vec(0u8..4, 0..8),
        new_nearby in proptest::collection::vec(0u8..4, 0..8),
    ) {
        futures::executor::block_on(check_snapshot_follow(old_visible, old_nearby, new_visible, new_nearby));
    }

    #[test]
    fn json_path_validation_equals_platform_validation(value in ".{0,100}") {
        let bytes = serde_json::to_vec(&serde_json::json!({"in_view": [&value]})).unwrap();
        let decoded = acyclic_harness::client::InterestDocument::decode(&bytes, bytes.len());
        proptest::prop_assert_eq!(decoded.is_ok(), acyclic_stream::StreamPath::new(&value).is_ok());
        if let Ok(document) = decoded {
            proptest::prop_assert_eq!(document.in_view[0].as_str(), value.as_str());
        }
    }

    #[test]
    fn layout_documents_round_trip_and_preserve_all_selections(
        visible in proptest::collection::vec(0u8..32, 0..32),
        nearby in proptest::collection::vec(0u8..32, 0..32),
    ) {
        let document = acyclic_harness::client::InterestDocument {
            in_view: super::selected(&visible).collect(),
            prefetch: super::selected(&nearby).collect(),
        };
        let bytes = serde_json::to_vec(&document).unwrap();
        let decoded = acyclic_harness::client::InterestDocument::decode(&bytes, bytes.len()).unwrap();
        proptest::prop_assert_eq!(decoded, document);
        proptest::prop_assert!(acyclic_harness::client::InterestDocument::decode(&bytes, bytes.len() - 1).is_err());
    }
}

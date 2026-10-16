//! A layout file drives the production interest loop and its shared reducer.
use acyclic_harness::{Error, Result,
    client::{SwarmChange, SwarmClient, SwarmClientLimits, SwarmViewLimits, QueryViewLimits},
    task_volume::{Channel, FileKey, Query, TaskVolume, Write}};
use acyclic_stream::{MemoryStream, ReadRequest, StreamPath, StreamProvider, wire, wire_codec};
use futures::{SinkExt as _, StreamExt as _, channel::mpsc};
use std::{rc::Rc, sync::Arc};

fn path(value: &str) -> StreamPath { StreamPath::new(value).unwrap() }
async fn put(volume: &TaskVolume<MemoryStream>, key: &str, bytes: &[u8]) {
    volume.append(volume.head().await.unwrap(), &Write::new(FileKey::parse(key).unwrap(), bytes.to_vec())).await.unwrap();
}
async fn selected(sent: &mut mpsc::Receiver<wire::SessionInterest>, agent: &str) -> wire::SessionSubscription {
    while let Some(update) = sent.next().await {
        if let Some(subscription) = update.upsert.into_iter().find(|sub| matches!(&sub.kind,
            Some(wire::session_subscription::Kind::Follow(follow)) if follow.delivery == wire::SessionDelivery::All as i32 && follow.path == format!("{agent}/files"))) {
            return subscription;
        }
    }
    panic!("interest stream ended before selecting {agent}")
}
async fn notice(changed: &mut mpsc::UnboundedReceiver<&'static str>, expected: &str) {
    loop {
        match changed.next().await {
            Some(actual) if actual == expected => return,
            Some(_) => (),
            None => panic!("view notifications ended before {expected}"),
        }
    }
}

#[tokio::test]
async fn view_file_writes_update_subscriptions_and_the_same_file_fold() -> Result<()> {
    let streams = Arc::new(MemoryStream::default());
    let a = Rc::new(TaskVolume::open(streams.clone(), path("swarm/a/files")).await.unwrap());
    let b = Rc::new(TaskVolume::open(streams.clone(), path("swarm/b/files")).await.unwrap());
    let config = TaskVolume::open(Arc::new(MemoryStream::default()), path("client/config")).await.unwrap();
    put(&config, "workspace/layout.json", br#"{"prefetch":["swarm/a"]}"#).await;
    let limits = SwarmClientLimits { agents: 4, selected: 1, layout_bytes: 1024, config_files: 4, snapshot_files: 16 };
    let mut client = SwarmClient::new(path("swarm"), limits, SwarmViewLimits {
        agents: 4, selected: 1, overview_values: 4, overview_bytes: 4096,
        query: QueryViewLimits { files: 16, segments: 16, bytes: 4096 }
    })?;
    let views = client.views();
    let (sink, mut sent) = mpsc::channel(4);
    let (mut frames, incoming) = mpsc::channel(4);
    let (notices, mut changed) = mpsc::unbounded();
    let consumer = client.follow(streams.as_ref(), &config,
        sink.sink_map_err(|error| Error::Storage(error.to_string())), incoming,
        |agent, maximum| {
            let volume = if agent == path("swarm/a") { a.clone() } else { assert_eq!(agent, path("swarm/b")); b.clone() };
            async move { volume.heads(&Query::Root, None, maximum).await }
        },
        |change| {
            // Reentrant renderer reads must see the completed fold, without a
            // writer guard held through the callback or the next source wait.
            views.read(|v| v.layout().visible().count())?;
            let kind = match change { SwarmChange::Layout => "layout", SwarmChange::Snapshot { .. } => "snapshot",
                SwarmChange::Data { overview: false, .. } => "file", SwarmChange::Data { overview: true, .. } => "status",
                SwarmChange::Watermark { .. } => "watermark" };
            notices.unbounded_send(kind).unwrap();
            Ok(())
        });
    let consumer = async {
        match consumer.await {
            Err(Error::Unsupported(message)) if message == "scenario complete" => Ok(()),
            other => other,
        }
    };
    let writer = async {
        let selected_a = selected(&mut sent, "swarm/a").await;
        notice(&mut changed, "snapshot").await;
        let cut = views.read(|v| v.cursor(&path("swarm/a")).unwrap().clone()).unwrap();
        assert_eq!(selected_a.resume.as_ref().unwrap().next, cut.position);
        let mut write = Write::new(FileKey::parse(".tasks/t/turns/1/chunks/1").unwrap(), b"live".to_vec());
        write.channel = Some(Channel::Text);
        a.append(a.head().await.unwrap(), &write).await.unwrap();
        let mut read = streams.read(ReadRequest { path: path("swarm/a/files"), from: cut.position, limit: 1 }).await.unwrap();
        let record = read.next().await.unwrap().unwrap();
        frames.send(Ok(wire::SessionFrame { channel_id: selected_a.channel.as_ref().unwrap().channel_id,
            generation: 1, body: Some(wire::session_frame::Body::Data(wire::SessionData {
                after: Some(wire_codec::session::record_cursor(record.sequence + 1, record.commit_id).unwrap()),
                accepted: record.sequence + 1, durable: 0,
                item: Some(wire::session_data::Item::Record(wire_codec::record_wire(record))) })) })).await.unwrap();
        notice(&mut changed, "file").await;
        let key = FileKey::parse(".tasks/t/turns/1/text.md").unwrap();
        let (cursor, pointer) = views.read(|views| {
            assert_eq!(views.file(&path("swarm/a"), &key).unwrap().segments[0].as_ref(), b"live");
            (views.cursor(&path("swarm/a")).unwrap().clone(), views.file(&path("swarm/a"), &key).unwrap().segments[0].as_ptr())
        }).unwrap();
        put(&config, "workspace/layout.json", br#"{"in_view":["swarm/a"]}"#).await;
        notice(&mut changed, "layout").await;
        views.read(|v| {
            assert_eq!(v.cursor(&path("swarm/a")), Some(&cursor));
            assert_eq!(v.file(&path("swarm/a"), &key).unwrap().segments[0].as_ptr(), pointer);
        }).unwrap();
        assert!(sent.try_recv().is_err(), "promotion must keep the same route");
        put(&config, "workspace/layout.json", br#"{"in_view":["swarm/b"]}"#).await;
        selected(&mut sent, "swarm/b").await;
        notice(&mut changed, "snapshot").await;
        views.read(|v| {
            assert!(v.cursor(&path("swarm/a")).is_none());
            assert!(v.cursor(&path("swarm/b")).is_some());
        }).unwrap();
        frames.send(Err(Error::Unsupported("scenario complete".into()))).await.unwrap();
        Ok::<_, Error>(())
    };
    futures::future::try_join(consumer, writer).await?;
    Ok(())
}

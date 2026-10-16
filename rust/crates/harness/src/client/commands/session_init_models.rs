use super::*;
use acyclic_stream::{MemoryStream, StreamPath};

const _: fn(acyclic_stream::query_sync::SyncMutex<()>) -> loom::sync::Mutex<()> = |lock| lock;

fn race<const RETRY: bool>(before_seed: bool) {
    loom::model(move || {
        loom::future::block_on(Box::pin(async {
            let volume = Arc::new(
                files::TaskVolume::open(
                    Arc::new(MemoryStream::default()),
                    StreamPath::new("command-init-model").unwrap(),
                )
                .await
                .unwrap(),
            );
            let mut commands =
                CommandSession::open(volume.clone(), uuid::Uuid::from_bytes([1; 16]), 8192, 32)
                    .await
                    .unwrap();
            let request = files::FileValue::new(
                files::FileKey::Session {
                    session: component(commands.session.to_string()).unwrap(),
                    file: files::TaskFile::Request(
                        component(OperationId::from_bytes([2; 16]).to_string()).unwrap(),
                    ),
                },
                br#"{"kind":"prompt","text":"hello","source":null,"mentions":[]}"#.to_vec(),
            );
            let seed = ui("config", "extra.json").unwrap();
            // Both linearization orders at the real snapshot/create handoff.
            // Publication runs on a Loom thread over the real instrumented
            // task-volume/provider locks. No substitute mutex or inbox exists.
            if before_seed {
                let writer = volume.clone();
                let request = request.clone();
                loom::thread::spawn(move || {
                    loom::future::block_on(Box::pin(async {
                        let cut = writer.head().await.unwrap();
                        writer.append(cut, &request).await.unwrap();
                    }))
                })
                .join()
                .unwrap();
            }
            commands
                .initialize_file::<RETRY>(seed.clone(), b"{}".to_vec())
                .await
                .unwrap();
            if !before_seed {
                let cut = volume.head().await.unwrap();
                volume.append(cut, &request).await.unwrap();
            }
            let restored = CommandSession::open(volume.clone(), commands.session, 8192, 32)
                .await
                .unwrap();
            assert_eq!(
                restored.files.get(&request.key),
                Some(&request),
                "startup lost a request"
            );
            assert_eq!(restored.files.get(&seed).unwrap().bytes.as_ref(), b"{}");
        }));
    });
}

#[test]
fn initialization_preserves_a_request_on_both_sides_of_seed_publication() {
    race::<true>(false);
    race::<true>(true);
}

#[test]
fn fail_on_initialization_contention_is_detected_negative_control() {
    assert!(std::panic::catch_unwind(|| race::<false>(true)).is_err());
}

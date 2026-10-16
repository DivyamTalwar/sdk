use super::*;
use crate::{
    OperationId,
    task_volume::{Channel, Component, FileKey, FileValue, TaskFile, TaskVolume},
};
use acyclic_stream::{MemoryStream, StreamPath};
use futures::FutureExt as _;
use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    // Snapshot plus follow equals reopening, independent of byte boundaries,
    // invalid UTF-8, channel and idempotent re-publication.
    #[test]
    fn task_files_snapshot_follow_and_reopen_share_the_same_views(
        chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 1..16), 1..7),
        cut in 0usize..7, reasoning in any::<bool>(),
    ) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let volume = TaskVolume::open(Arc::new(MemoryStream::default()), StreamPath::new("file-client").unwrap()).await.unwrap();
            let task = crate::TaskId::from_bytes([1;16]);
            let owner = wire::Authority { kind: wire::AggregateKind::Task as i32, id: task.to_string() };
            let key = format!("AGGREGATE_KIND_TASK:{task}");
            let mut generation = volume.snapshot(&Query::Tasks, 32).await.unwrap().generation;
            let files: Vec<_> = chunks.iter().enumerate().map(|(index, bytes)| FileValue {
                key: FileKey::Task { task: Component::new(task.to_string()).unwrap(), file: TaskFile::Chunk { turn: 0, sequence: index as u64 } },
                channel: Some(if reasoning { Channel::Reasoning } else { Channel::Text }), bytes: bytes.clone().into(),
            }).collect();
            let cut = cut.min(files.len());
            for file in &files[..cut] { generation = volume.append(generation, file).await.unwrap(); }
            let mut live = TaskFileClient::observe(&volume, &owner, 32, 4096).await.unwrap();
            for file in &files[cut..] {
                generation = volume.append(generation, file).await.unwrap();
                tokio::task::unconstrained(live.next_change()).now_or_never().expect("accepted cut must be ready").unwrap();
                prop_assert_eq!(volume.append(generation, file).await.unwrap(), generation);
                prop_assert!(tokio::task::unconstrained(live.next_change()).now_or_never().is_none());
            }
            let replay = TaskFileClient::observe(&volume, &owner, 32, 4096).await.unwrap();
            prop_assert_eq!(&live.views().transcripts, &replay.views().transcripts);
            let bytes: Vec<u8> = chunks.into_iter().flatten().collect();
            let transcript = &live.views().transcripts[&key];
            let text = if reasoning { &transcript.reasoning } else { &transcript.streaming_text };
            let expected = String::from_utf8_lossy(&bytes);
            prop_assert_eq!(text.as_str(), expected.as_ref());
            prop_assert_eq!(transcript.streaming_turn, Some(OperationId::from_bytes([0;16])));
            Ok(())
        })?;
    }
}

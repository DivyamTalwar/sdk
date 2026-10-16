//! A filtered checkpoint cannot impersonate the requested stop/cancel file.
use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn unrelated_publications_never_complete_a_file_wait(identity in any::<[u8;16]>()) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let files = TaskVolume::open(Arc::new(acyclic_stream::MemoryStream::default()),
                StreamPath::new("control").unwrap()).await.unwrap();
            let instance = uuid::Uuid::from_bytes(identity);
            let key = stop_key(instance).unwrap();
            let mut changed = identity;
            changed[0] ^= 1;
            let unrelated = FileValue::new(stop_key(uuid::Uuid::from_bytes(changed)).unwrap(),b"{}".as_slice());
            let mut waiting = Box::pin(stopped(&files,instance));
            assert!(futures::poll!(waiting.as_mut()).is_pending());
            files.append(files.head().await.unwrap(),&unrelated).await.unwrap();
            assert!(futures::poll!(waiting.as_mut()).is_pending());
            let wanted = FileValue::new(key,b"{}".as_slice());
            files.append(files.head().await.unwrap(),&wanted).await.unwrap();
            waiting.await.unwrap();
        });
    }
}

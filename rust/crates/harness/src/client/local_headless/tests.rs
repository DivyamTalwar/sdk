use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    // Canonical file JSON preserves the exact selected identity and payload;
    // another session or an execution-owned key is rejected before publication.
    #[test]
    fn local_jsonl_requests_round_trip_without_authority_expansion(
        identity in any::<[u8;16]>(), text in ".{1,128}",
    ) {
        let operation = OperationId::from_bytes(identity);
        let request = prepare_local_prompt(operation, &text).unwrap();
        let line = serde_json::to_string(&request).unwrap();
        prop_assert_eq!(parse_local_request(&line, operation).unwrap(), request.clone());
        let mut foreign = identity; foreign[0] ^= 1;
        prop_assert!(matches!(parse_local_request(&line, OperationId::from_bytes(foreign)), Err(Error::Unauthorized(_))));
        for make in [TaskFile::ExecutionClaim, TaskFile::ExecutionResult] {
            let mut forbidden = request.clone();
            forbidden.key = key(operation, make).unwrap();
            prop_assert!(matches!(parse_local_request(&serde_json::to_string(&forbidden).unwrap(), operation), Err(Error::Unauthorized(_))));
        }
    }

    // Every settled result, including bounded output omission, preserves its
    // explicit completion class instead of trying to classify error strings.
    #[test]
    fn completion_class_survives_bounded_outcome_files(count in 0usize..4096) {
        for kind in 0u8..5 { futures::executor::block_on(async {
            use crate::task_volume::TaskVolume;
            let volume = Arc::new(TaskVolume::open(Arc::new(acyclic_stream::MemoryStream::default()), acyclic_stream::StreamPath::new("headless-outcomes").unwrap()).await.unwrap());
            let cut = volume.snapshot(&Query::Root, 32).await.unwrap();
            volume.append(cut.generation, &FileValue::new(FileKey::parse("commands/catalog.json").unwrap(), b"[]".to_vec())).await.unwrap();
            let operation = OperationId::new();
            let mut commands = super::super::CommandSession::open(volume, uuid::Uuid::from_bytes(operation.into_bytes()), 1024, 32).await.unwrap();
            commands.publish_resolution(CommandResolution::Prompt { text: "prompt".into(), source: None, mentions: Vec::new() }).await.unwrap();
            let request = commands.next_request().await.unwrap();
            let expected = [HeadlessOutcome::Completed, HeadlessOutcome::Failed, HeadlessOutcome::Cancelled, HeadlessOutcome::Blocked, HeadlessOutcome::Indeterminate][kind as usize];
            let result = commands.consume(&request, |actual,_,_| async move {
                match kind {
                    0 => Ok(serde_json::json!({"text":"x".repeat(count)})),
                    1 => Err(Error::Storage("x".repeat(count))),
                    2 => Err(Error::InteractionRejected(crate::InteractionRejection::Cancelled)),
                    3 => Err(Error::InteractionRejected(crate::InteractionRejection::Declined)),
                    _ => Err(Error::Indeterminate(actual)),
                }
            }).await;
            assert_eq!(result.is_ok(), kind == 0);
            let saved = commands.output_page(0).unwrap().unwrap();
            let bytes = serde_json::to_vec(&saved).unwrap();
            assert!(bytes.len() <= 1024);
            assert_eq!(super::super::command_output::completion(&bytes).unwrap(), expected);
        }); }
    }
}

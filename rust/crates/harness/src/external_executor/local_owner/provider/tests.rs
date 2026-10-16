//! Requests cannot mutate owner results or internal scheduler streams.
use super::*;
use proptest::prelude::*;

#[test]
fn client_commit_authority_never_widens_to_owner_paths() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(
        runtime
            .block_on(LocalStream::open(directory.path(), Default::default()))
            .unwrap(),
    );
    let client = ClientStream::workspace(Arc::clone(&local));
    let writable = StreamPath::new("external/requests").unwrap();
    runtime
        .block_on(local.append(AppendRequest {
            path: writable,
            records: vec![b"request header".as_slice().into()],
            if_tail: Some(0),
            idempotency_key: None,
        }))
        .unwrap();
    let protected = StreamPath::new("external/results").unwrap();
    runtime
        .block_on(local.append(AppendRequest {
            path: protected.clone(),
            records: vec![b"owner".as_slice().into()],
            if_tail: Some(0),
            idempotency_key: None,
        }))
        .unwrap();
    let strategy = prop_oneof![
        Just("external/requests".to_owned()),
        Just("external/results".to_owned()),
        Just("owner/control".to_owned()),
        "[a-z]{1,24}/[a-z]{1,24}",
    ];
    proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(24))
        .run(&strategy, |name| {
            let path = StreamPath::new(&name).unwrap();
            prop_assert_eq!(
                runtime.block_on(
                    client.inspect_idempotency(
                        IdempotencyKey::new(name.as_bytes().to_vec()).unwrap()
                    )
                ),
                Err(StreamError::AccessDenied)
            );
            let request = CommitRequest {
                conditions: vec![CommitCondition::Tail {
                    path: path.clone(),
                    expected: runtime.block_on(local.tail(path.clone())).unwrap_or(0),
                }],
                mutations: vec![CommitMutation::Append {
                    path: path.clone(),
                    records: vec![b"caller".as_slice().into()],
                }],
                idempotency_key: IdempotencyKey::new(uuid::Uuid::new_v4().as_bytes().to_vec())
                    .unwrap(),
            };
            let result = runtime.block_on(client.commit(request));
            if name == "external/requests" {
                prop_assert!(
                    matches!(result, Ok(CommitOutcome::Committed(_))),
                    "{result:?}"
                );
            } else {
                prop_assert_eq!(result, Err(StreamError::AccessDenied));
            }
            prop_assert_eq!(runtime.block_on(local.tail(protected.clone())).unwrap(), 1);
            prop_assert_eq!(
                runtime.block_on(client.tail(StreamPath::new("scheduler/private").unwrap())),
                Err(StreamError::AccessDenied)
            );
            Ok(())
        })
        .unwrap();
}

//! The local owner consumes actual authorized request files through stock turns.
use super::*;
use crate::{
    client::{CommandResolution, CommandSession, consume_local_command},
    task_volume::{Component, FileKey, FileValue, Query, TaskFile},
};

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4))]
    // Every accepted prompt has one stock admission/provider call, and its
    // exact outcome survives both duplicate consumption and owner reopen.
    #[test]
    fn prompt_request_admits_once_and_reopens_its_file_outcome(prompt in "[a-z][a-z ]{0,48}") {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = runtime.block_on(roundtrip(prompt));
        prop_assert!(result.is_ok(), "{result:?}");
    }

    // Foreign task identities cannot dispatch a local model request, even if
    // the caller may write a request file in the containing private volume.
    #[test]
    fn foreign_task_requests_cannot_admit_a_local_model_turn(bytes in any::<[u8; 16]>()) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = runtime.block_on(foreign_request(bytes));
        prop_assert!(result.is_ok(), "{result:?}");
    }
}

async fn roundtrip(prompt: String) -> Result<()> {
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    let provider = text();
    let harness = LocalHarness::open(
        LocalHarnessConfig::new(directory.path(), (model()?, provider.clone()))
            .without_default_tools(),
    )
    .await?;
    let session = uuid::Uuid::new_v4();
    let operation = OperationId::new();
    let request = FileValue::new(
        FileKey::Task {
            task: Component::new(harness.client_owner().id)?,
            file: TaskFile::Request(Component::new(operation.to_string())?),
        },
        serde_json::to_vec(&CommandResolution::Prompt {
            text: prompt,
            source: None,
            mentions: Vec::new(),
        })
        .map_err(super::super::storage)?,
    );
    let files = harness.task_files();
    let snapshot = files.snapshot(&Query::Root, 128).await?;
    files.write(snapshot.generation, &request).await?;
    let mut owner = CommandSession::open(Arc::clone(&files.inner), session, 65_536, 128).await?;
    let observed = owner.next_request().await?;
    assert_eq!(observed, request);
    let output = consume_local_command(&mut owner, &harness, &observed)
        .await?
        .unwrap();
    assert_eq!(output["text"], "done");
    let projected =
        crate::client::TaskFileClient::observe(files, &harness.client_owner(), 128, 65_536).await?;
    let key = format!("AGGREGATE_KIND_TASK:{}", harness.client_owner().id);
    assert_eq!(projected.views().transcripts[&key].streaming_text, "done");
    drop(projected);
    assert_eq!(provider.requests().len(), 1);
    assert!(
        consume_local_command(&mut owner, &harness, &observed)
            .await?
            .is_none()
    );
    assert_eq!(provider.requests().len(), 1);
    drop(owner);
    drop(harness);

    let harness = LocalHarness::open(
        LocalHarnessConfig::new(directory.path(), (model()?, provider.clone()))
            .without_default_tools(),
    )
    .await?;
    let mut owner = CommandSession::open(
        Arc::clone(&harness.task_files().inner),
        session,
        65_536,
        128,
    )
    .await?;
    assert!(owner.take_request().is_none());
    assert!(
        consume_local_command(&mut owner, &harness, &observed)
            .await?
            .is_none()
    );
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(owner.output_page(0)?.unwrap()["ok"], output);
    Ok(())
}

async fn foreign_request(mut bytes: [u8; 16]) -> Result<()> {
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    let provider = scripted([]);
    let harness = LocalHarness::open(
        LocalHarnessConfig::new(directory.path(), (model()?, provider.clone()))
            .without_default_tools(),
    )
    .await?;
    if crate::TaskId::from_bytes(bytes).to_string() == harness.client_owner().id {
        bytes[0] ^= 1;
    }
    let request = FileValue::new(
        FileKey::Task {
            task: Component::new(crate::TaskId::from_bytes(bytes).to_string())?,
            file: TaskFile::Request(Component::new(OperationId::new().to_string())?),
        },
        serde_json::to_vec(&CommandResolution::Prompt {
            text: "foreign request".into(),
            source: None,
            mentions: Vec::new(),
        })
        .map_err(super::super::storage)?,
    );
    let files = harness.task_files();
    let snapshot = files.snapshot(&Query::Root, 128).await?;
    files.write(snapshot.generation, &request).await?;
    let mut owner =
        CommandSession::open(Arc::clone(&files.inner), uuid::Uuid::new_v4(), 65_536, 128).await?;
    assert!(matches!(
        consume_local_command(&mut owner, &harness, &request).await,
        Err(Error::Unauthorized(_))
    ));
    assert_eq!(provider.requests().len(), 0);
    Ok(())
}

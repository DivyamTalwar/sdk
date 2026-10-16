//! Actual local admission through the shared headless entry points.
use super::*;
use crate::client::{HeadlessOutcome, connect_local, local_prompt, run_headless, run_jsonl};
use std::io::{self, Write};

async fn local() -> Result<(
    tempfile::TempDir,
    Arc<LocalHarness>,
    Arc<ScriptedModelProvider>,
)> {
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    let provider = text();
    let harness = Arc::new(
        LocalHarness::open(
            crate::test_support::local_harness_config(
                directory.path(),
                (model()?, provider.clone()),
            )?
            .without_default_tools(),
        )
        .await?,
    );
    Ok((directory, harness, provider))
}
async fn close(harness: Arc<LocalHarness>) -> Result<()> {
    Arc::try_unwrap(harness)
        .map_err(|_| Error::Conflict("headless client retained its host".into()))?
        .close()
        .await
}
fn final_text(output: &[u8]) -> String {
    let changes = std::str::from_utf8(output)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str::<crate::task_volume::Change>(line).unwrap());
    changes
        .flat_map(|change| change.file.into_iter().chain(change.files))
        .filter(|file| {
            matches!(
                file.key,
                crate::task_volume::FileKey::Session {
                    file: crate::task_volume::TaskFile::ExecutionResult(_),
                    ..
                }
            )
        })
        .map(|file| {
            serde_json::from_slice::<Value>(&file.bytes).unwrap()["ok"]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::test]
async fn prompt_runs_through_files_and_reopening_does_not_resend() -> Result<()> {
    let (_directory, harness, provider) = local().await?;
    let operation = OperationId::new();
    let (mut client, target) = connect_local(harness.clone(), operation).await?;
    assert_eq!(provider.requests().len(), 0);
    let command = local_prompt(&harness, operation, "hello").await?;
    let mut output = Vec::new();
    assert_eq!(
        run_headless(&mut client, command.clone(), target, &mut output).await?,
        HeadlessOutcome::Completed
    );
    assert_eq!(final_text(&output), "done");
    assert_eq!(provider.requests().len(), 1);
    drop(client);
    let (mut client, target) = connect_local(harness.clone(), operation).await?;
    let mut restored = Vec::new();
    assert_eq!(
        run_headless(&mut client, command, target, &mut restored).await?,
        HeadlessOutcome::Completed
    );
    assert_eq!(provider.requests().len(), 1);
    drop(client);
    close(harness).await
}

#[tokio::test]
async fn jsonl_eof_drains_the_admitted_file_request() -> Result<()> {
    let (_directory, harness, provider) = local().await?;
    let operation = OperationId::new();
    let command = local_prompt(&harness, operation, "hello").await?;
    let line = serde_json::to_string(&command).map_err(super::super::storage)?;
    let (mut client, target) = connect_local(harness.clone(), operation).await?;
    let mut output = Vec::new();
    let input = futures::stream::iter([Ok(line)]);
    assert_eq!(
        run_jsonl(&mut client, target, input, &mut output).await?,
        HeadlessOutcome::Completed
    );
    assert_eq!(final_text(&output), "done");
    assert_eq!(provider.requests().len(), 1);
    drop(client);
    close(harness).await
}

struct RejectOutcome {
    rejected: bool,
}
impl Write for RejectOutcome {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.rejected
            && bytes
                .windows(b"ExecutionResult".len())
                .any(|part| part == b"ExecutionResult")
        {
            self.rejected = true;
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn stdout_failure_keeps_the_exact_pending_outcome_and_owner() -> Result<()> {
    let (_directory, harness, provider) = local().await?;
    let operation = OperationId::new();
    let command = local_prompt(&harness, operation, "hello").await?;
    let (mut client, target) = connect_local(harness.clone(), operation).await?;
    let mut broken = RejectOutcome { rejected: false };
    assert!(
        run_headless(&mut client, command.clone(), target, &mut broken)
            .await
            .is_err()
    );
    assert!(broken.rejected);
    assert_eq!(provider.requests().len(), 1);
    let mut output = Vec::new();
    assert_eq!(
        run_headless(&mut client, command, target, &mut output).await?,
        HeadlessOutcome::Completed
    );
    // The initial snapshot was already published; retry emits the pending change.
    let change: crate::task_volume::Change =
        serde_json::from_slice(output.split(|byte| *byte == b'\n').next().unwrap())
            .map_err(super::super::storage)?;
    assert!(matches!(
        change.file.unwrap().key,
        crate::task_volume::FileKey::Session {
            file: crate::task_volume::TaskFile::ExecutionResult(_),
            ..
        }
    ));
    assert_eq!(provider.requests().len(), 1);
    drop(client);
    close(harness).await
}

#[tokio::test]
async fn opening_and_empty_jsonl_do_not_start_a_model() -> Result<()> {
    let (_directory, harness, provider) = local().await?;
    let (mut client, target) = connect_local(harness.clone(), OperationId::new()).await?;
    assert_eq!(provider.requests().len(), 0);
    assert!(matches!(
        run_jsonl(
            &mut client,
            target,
            futures::stream::empty(),
            &mut Vec::new()
        )
        .await,
        Err(Error::NotFound(_))
    ));
    assert_eq!(provider.requests().len(), 0);
    drop(client);
    close(harness).await
}

#[tokio::test]
async fn task_files_share_the_snapshot_follow_and_reopen_cut() -> Result<()> {
    use crate::task_volume::{Component, FileKey, FileValue, Query, TaskFile, ToolFile};
    let (_directory, harness, provider) = local().await?;
    let operation = OperationId::new();
    let task = Component::new(crate::runtime::task_id_for_admission(operation).to_string())?;
    let volume = &harness.task_files().inner;
    let file = |task: Component, call: &str| -> Result<FileValue> {
        Ok(FileValue::new(
            FileKey::Task {
                task,
                file: TaskFile::Tool {
                    turn: 0,
                    call: Component::new(call)?,
                    file: ToolFile::Result,
                },
            },
            call.as_bytes().to_vec(),
        ))
    };
    let before = file(task.clone(), "before")?;
    let after = file(task.clone(), "after")?;
    let foreign = file(Component::new("unrelated")?, "foreign")?;
    let cut = volume.snapshot(&Query::Root, 4096).await?;
    let cut = volume.append(cut.generation, &before).await?;
    let cut = volume.append(cut, &foreign).await?;
    let (mut client, target) = connect_local(harness.clone(), operation).await?;
    volume.append(cut, &after).await?;
    let mut output = Vec::new();
    assert_eq!(
        run_headless(
            &mut client,
            local_prompt(&harness, operation, "hello").await?,
            target,
            &mut output
        )
        .await?,
        HeadlessOutcome::Completed
    );
    let mut lines = std::str::from_utf8(&output).unwrap().lines();
    let initial: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let head_keys: Vec<FileKey> = initial[1]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| serde_json::from_value(file["key"].clone()).unwrap())
        .collect();
    assert!(head_keys.contains(&before.key));
    assert!(!head_keys.contains(&foreign.key));
    let changes: Vec<crate::task_volume::Change> = lines
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let keys: Vec<_> = changes
        .iter()
        .flat_map(|change| change.file.iter().chain(&change.files))
        .map(|file| &file.key)
        .collect();
    assert!(keys.contains(&&after.key));
    assert!(!keys.contains(&&foreign.key));
    drop(client);
    let (mut client, target) = connect_local(harness.clone(), operation).await?;
    let mut restored = Vec::new();
    run_headless(
        &mut client,
        local_prompt(&harness, operation, "hello").await?,
        target,
        &mut restored,
    )
    .await?;
    let initial: Value = serde_json::from_str(
        std::str::from_utf8(&restored)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let head_keys: Vec<FileKey> = initial[1]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| serde_json::from_value(file["key"].clone()).unwrap())
        .collect();
    assert!(head_keys.contains(&before.key));
    assert!(head_keys.contains(&after.key));
    assert!(!head_keys.contains(&foreign.key));
    assert_eq!(provider.requests().len(), 1);
    drop(client);
    close(harness).await
}

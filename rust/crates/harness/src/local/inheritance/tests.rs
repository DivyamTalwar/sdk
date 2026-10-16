use super::*;
use crate::{
    client::{CommandControl, CommandRequest, CommandResolution, CommandTaskScope},
    runtime::RuntimeTask,
    test_support::{ScriptedModelProvider, admission_work::measure},
};
use proptest::prelude::*;

async fn open(directory: &Path) -> Result<LocalHarness> {
    let volume = super::super::tests::volume(directory).await?;
    let config = super::super::tests::volume_config(
        directory,
        volume,
        Arc::new(ScriptedModelProvider::new([])),
    )?
    .without_default_tools()
    .compaction(crate::context::CompactionPolicy::Disabled);
    LocalHarness::open(config).await
}
async fn child(local: &LocalHarness, parent: Option<TaskId>) -> Result<TaskId> {
    match local
        .submit_to(OperationId::new(), parent, "continue")
        .await?
    {
        Admission::Accepted(RuntimeTask::Durable { task_id, .. }) => Ok(task_id),
        _ => panic!("admission rejected"),
    }
}
async fn write(local: &LocalHarness, task: TaskId, name: &str, value: Value) -> Result<()> {
    let key = key(task, name)?;
    let cut = local
        .task_files
        .inner
        .snapshot(&Query::File(key.clone()), 1)
        .await
        .map_err(storage)?;
    local
        .task_files
        .inner
        .append(
            cut.generation,
            &FileValue::new(key, serde_json::to_vec(&value).map_err(storage)?),
        )
        .await
        .map_err(storage)?;
    Ok(())
}
// Drives each production consumer once per admitted turn. No provider/model
// executes; the counter is at TaskStateProvider::observe_admission itself.
async fn conversation(turns: usize, full_collect: bool) -> Result<u64> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let local = open(directory.path()).await?;
    let (result, calls) = measure(async {
        let foreign = TaskId::from_bytes([0xff; 16]);
        CommandControl::change(
            &local.task_files.inner,
            Some(foreign),
            &[],
            Some(true),
            local.limits.file_bytes,
        )
        .await?;
        let mut parent = None;
        let mut root = None;
        for _ in 0..turns {
            let task = child(&local, parent).await?;
            let root = *root.get_or_insert(task);
            if full_collect {
                // The removed lineage() implementation, using the same owner
                // and ancestors stream. This work must violate the paired bound.
                let record = local.runtime.task_host().observe_admission(task).await?;
                let context = local
                    .runtime
                    .harness()
                    .durable_context(task, record.operation_id, None)
                    .await?;
                let _ =
                    futures::TryStreamExt::try_collect::<Vec<_>>(context.ancestors(task)?).await?;
            }
            local.stock_commands(Some(task)).await?;
            assert_eq!(local.prompt_context(Some(task), "next").await?, "next");
            assert!(local.command_has_ancestor(Some(task), root).await?);
            assert!(!local.command_has_ancestor(Some(task), foreign).await?);
            local.wait_command_control(task).await?;
            parent = Some(task);
        }
        Ok::<_, Error>(())
    })
    .await;
    result?;
    Ok(calls)
}
async fn paired(turns: usize) -> Result<()> {
    let small = conversation(turns, false).await?;
    let large = conversation(2 * turns, false).await?;
    assert!(
        large <= 2 * small + 1,
        "nonlinear admission reads: {turns}={small}, {}={large}",
        2 * turns
    );
    let broken_small = conversation(turns, true).await?;
    let broken_large = conversation(2 * turns, true).await?;
    assert!(
        broken_large > 2 * broken_small + 1,
        "full-collect negative control did not fail the linear bound"
    );
    println!(
        "N={turns}: {small}/{large} observe_admission calls; full-collect {broken_small}/{broken_large}"
    );
    Ok(())
}
#[tokio::test]
async fn whole_conversation_admission_work_grows_linearly_and_detects_full_collect() -> Result<()> {
    paired(8).await
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(4))]
    #[test]
    fn linear_admission_work_shrinks_across_conversation_lengths(turns in 2usize..9) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(paired(turns)).unwrap();
    }
}
#[tokio::test]
async fn inherited_bindings_are_pinned_before_admission_and_survive_reopening() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let local = open(directory.path()).await?;
    let root = child(&local, None).await?;
    write(&local, root, "settings", json!({"model":"first"})).await?;
    write(&local, root, "context", json!({"summary":"remember first"})).await?;
    let operation = OperationId::new();
    let pending = crate::runtime::task_id_for_admission(operation);
    local.prepare_inheritance(pending, Some(root)).await?;
    let next = child(&local, Some(root)).await?;
    write(&local, root, "settings", json!({"model":"later"})).await?;
    write(&local, root, "context", json!({"summary":"later summary"})).await?;
    let later = child(&local, Some(root)).await?;
    drop(local);
    let local = open(directory.path()).await?;
    let admitted = local.submit_to(operation, Some(root), "continue").await?;
    assert!(
        matches!(admitted, Admission::Accepted(RuntimeTask::Durable {task_id, ..}) if task_id == pending)
    );
    let record = local.runtime.task_host().observe_admission(pending).await?;
    let command: WorkflowCommand =
        serde_json::from_value(record.input["model"].clone()).map_err(storage)?;
    let context = local
        .runtime
        .harness()
        .durable_context(pending, operation, None)
        .await?;
    let bytes = context.read_file(&command.payload, None).await?;
    let input: crate::filesystem::ModelTaskCommand =
        serde_json::from_slice(&bytes).map_err(storage)?;
    assert!(
        matches!(input.input, ModelContent::Text(text) if text.contains("remember first") && !text.contains("later summary")),
        "resumed admission composed a prompt from a newer ancestor instead of its retained binding"
    );
    for (task, model, summary) in [
        (next, "first", "remember first"),
        (later, "later", "later summary"),
    ] {
        let inherited = local.resolved_inheritance(task).await?;
        assert_eq!(inherited["model"], model);
        assert_eq!(inherited["summary"], summary);
    }
    write(&local, next, "settings", json!({"model":"child override"})).await?;
    let grandchild = child(&local, Some(next)).await?;
    assert_eq!(
        local.resolved_inheritance(grandchild).await?["model"],
        "child override"
    );
    // Subtree controls remain live, including scopes added after admission.
    local
        .dispatch_command(
            OperationId::new(),
            Some(root),
            CommandResolution::Submit {
                request: CommandRequest::Pause {
                    scope: CommandTaskScope::Subtree,
                },
            },
        )
        .await?;
    let mut waiting = Box::pin(local.wait_command_control(grandchild));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), waiting.as_mut())
            .await
            .is_err()
    );
    local
        .dispatch_command(
            OperationId::new(),
            Some(next),
            CommandResolution::Submit {
                request: CommandRequest::Resume {
                    scope: CommandTaskScope::Subtree,
                },
            },
        )
        .await?;
    waiting.await?;
    assert!(local.command_has_ancestor(Some(grandchild), root).await?);
    assert!(!local.command_has_ancestor(Some(later), next).await?);
    Ok(())
}

#[tokio::test]
async fn legacy_admissions_are_upgraded_once_then_reopen_without_ancestry_reads() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let local = open(directory.path()).await?;
    let root = child(&local, None).await?;
    write(&local, root, "settings", json!({"model":"inherited"})).await?;
    write(
        &local,
        root,
        "context",
        json!({"summary":"retained context"}),
    )
    .await?;
    let definition = local
        .runtime
        .harness()
        .task::<Value, crate::executor::TurnOutput>("acyclic.stock_turn@1")?;
    let mut task = root;
    for _ in 0..4 {
        let record = local.runtime.task_host().observe_admission(task).await?;
        let context = local
            .runtime
            .harness()
            .durable_context(task, record.operation_id, None)
            .await?;
        let Admission::Accepted(RuntimeTask::Durable { task_id, .. }) = context
            .admit(OperationId::new(), &definition, record.input)
            .await?
        else {
            panic!("legacy admission");
        };
        task = task_id;
    }
    let (value, cold) = measure(local.resolved_inheritance(task)).await;
    assert_eq!(value?["summary"], "retained context");
    assert_eq!(cold, 5, "one lookup plus the four missing parent edges");
    drop(local);
    let local = open(directory.path()).await?;
    let (value, warm) = measure(local.resolved_inheritance(task)).await;
    assert_eq!(value?["model"], "inherited");
    assert_eq!(warm, 0);
    Ok(())
}

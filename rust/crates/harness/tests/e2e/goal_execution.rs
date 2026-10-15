use super::{TestResult, execution, goals::GoalCase, matchers};
use acyclic_harness::{
    Harness, OperationId, TaskId,
    context::CompactionPolicy,
    filesystem::MemoryHarnessStorage,
    goal::{GoalContinuation, GoalView},
    model::Model,
    test_support::ScriptedModelProvider,
    tool::files,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

async fn read_goal(storage: &MemoryHarnessStorage) -> acyclic_harness::Result<GoalView> {
    let (source, bytes) = storage
        .read_delegated_path(storage.owner_scope(), "", "GOAL.md", None)
        .await?;
    GoalView::from_file(source, &bytes, 4096)
}

async fn run_turn(
    storage: &MemoryHarnessStorage,
    bundle: &Harness,
    provider: &ScriptedModelProvider,
    operation: OperationId,
    case: &GoalCase,
) -> TestResult {
    let input = storage
        .stage(
            operation,
            &format!("turns/{operation}/prompt.txt"),
            case.prompt.as_bytes(),
            "text/plain",
            "prompt.txt",
        )
        .await?;
    let output = storage
        .run_conversation(bundle, operation, input.clone(), Vec::new(), case.max_steps)
        .await?;
    if output.text != case.expect.text {
        return Err(format!(
            "goal turn text: expected {:?}, got {:?}",
            case.expect.text, output.text
        )
        .into());
    }
    let dispatches = provider.dispatches();
    let replay = storage
        .run_conversation(bundle, operation, input, Vec::new(), case.max_steps)
        .await?;
    if replay != output || provider.dispatches() != dispatches {
        return Err("replayed goal admission dispatched again or changed output".into());
    }
    Ok(())
}

// Exercises the real executor, ordinary file tool and continuation policy.
// Automatic runtime admission belongs to LocalHarness, not this test driver.
pub(super) async fn run(mut case: GoalCase) -> TestResult {
    let storage = execution::workspace(&case.seed).await?;
    let initial = if case.seed.contains_key("GOAL.md") {
        Some(
            storage
                .read_delegated_path(storage.owner_scope(), "", "GOAL.md", None)
                .await?,
        )
    } else {
        None
    };
    let provider = Arc::new(ScriptedModelProvider::new(std::mem::take(&mut case.turns)));
    let bundle = storage
        .builder()
        .model(
            Model::new("scripted".into(), "goal".into(), "1".into(), Value::Null)?,
            provider.clone(),
        )
        .grant("model:generate")
        .tool(files::write_file()?)?
        .grant("tool:call:acyclic.write_file")
        .compaction(CompactionPolicy::Disabled)
        .build()?;
    let task = TaskId::new();
    let mut settled_turn = OperationId::new();
    run_turn(&storage, &bundle, &provider, settled_turn, &case).await?;
    let mut admitted = BTreeSet::new();
    let mut actions = Vec::new();
    for _ in 0..case.settles {
        let view = read_goal(&storage).await?;
        if case.expect.unchanged_goal
            && Some(view.revision())
                != initial
                    .as_ref()
                    .map(|(source, _)| *source.descriptor().sha256())
        {
            return Err("goal changed before the continuation policy settled it".into());
        }
        let action = case
            .policy
            .on_settle(&view, task, settled_turn, admitted.len() as u32);
        assert_eq!(
            action,
            case.policy
                .on_settle(&view, task, settled_turn, admitted.len() as u32)
        );
        match action {
            Some(GoalContinuation::Continue(operation)) => {
                if !admitted.insert(operation) {
                    return Err("repeated continuation identity".into());
                }
                actions.push("continue");
                run_turn(&storage, &bundle, &provider, operation, &case).await?;
                settled_turn = operation;
            }
            Some(GoalContinuation::Block(contents)) => {
                actions.push("block");
                storage
                    .stage(
                        view.continuation_operation(task, settled_turn)
                            .ok_or("missing cap identity")?,
                        "GOAL.md",
                        contents.as_bytes(),
                        "text/markdown",
                        "GOAL.md",
                    )
                    .await?;
                let blocked = read_goal(&storage).await?;
                assert_eq!(blocked.markdown, view.markdown);
                assert_eq!(
                    case.policy
                        .on_settle(&blocked, task, settled_turn, admitted.len() as u32),
                    None
                );
                break;
            }
            None => {
                actions.push("stop");
                break;
            }
        }
    }
    let requests = provider.requests();
    let prompt = requests
        .first()
        .ok_or("missing goal model request")?
        .messages
        .iter()
        .flat_map(|message| message.content.file_refs())
        .find(|file| file.display_name() == "prompt.txt")
        .ok_or("goal request omitted the prompt file")?;
    if storage.read(prompt).await?.as_ref() != case.expect.request_prompt.as_bytes() {
        return Err("model saw different goal prompt bytes".into());
    }
    if provider.remaining() != 0 || !provider.reconciliations().is_empty() {
        return Err("unused goal script or unexpected reconciliation".into());
    }
    let result = json!({"continuations":admitted.len(), "settles":actions.len(),
        "actions":actions, "requests":requests.len()});
    if !matchers::subset(&case.expect.result, &result) {
        return Err(format!("goal result: expected {}, got {result}", case.expect.result).into());
    }
    match (
        case.expect.view,
        case.expect.error,
        read_goal(&storage).await,
    ) {
        (Some(expected), None, Ok(actual))
            if matchers::subset(&expected, &serde_json::to_value(&actual)?) => {}
        (None, Some(expected), Err(actual)) if actual.to_string().contains(&expected) => {}
        actual => return Err(format!("unexpected goal view: {actual:?}").into()),
    }
    if let Some(expected) = case.expect.requests {
        let requests = requests
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        matchers::ordered(&expected, &requests, "goal requests")?;
    }
    execution::check_files(&case.expect.files, &storage).await?;
    if let Some((source, bytes)) = initial {
        assert_eq!(storage.read(&source).await?.as_ref(), bytes.as_ref());
    }
    Ok(())
}

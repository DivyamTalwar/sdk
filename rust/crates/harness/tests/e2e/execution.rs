use super::{
    TestResult, matchers,
    scenario::{Expectations, Scenario},
};
use acyclic_harness::{
    AgentId, OperationId,
    context::{CompactionPolicy, ContextPipeline},
    executor::{ExecutionEvent, Executor, StockExecutor, TurnInput},
    filesystem::MemoryHarnessStorage,
    model::{Model, ModelContent},
    test_support::ScriptedModelProvider,
    tool::ToolRegistry,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
pub(super) async fn run(case: Scenario) -> TestResult {
    let limits = case.execution_limits()?;
    let storage = MemoryHarnessStorage::new(AgentId::new(), 1_048_576).await?;
    for (path, contents) in case.seed {
        storage
            .stage(
                OperationId::new(),
                &path,
                contents.as_bytes(),
                "text/plain",
                path.rsplit('/').next().unwrap_or_default(),
            )
            .await?;
    }
    let provider = Arc::new(ScriptedModelProvider::new(case.turns));
    let executor = StockExecutor::new(
        Model::new(
            "scripted".into(),
            "scenario".into(),
            "1".into(),
            Value::Null,
        )?,
        provider.clone(),
        ContextPipeline::default(),
        ToolRegistry::default(),
    )
    .with_compaction_policy(CompactionPolicy::Disabled)
    .with_limits(limits);
    let input = TurnInput {
        operation_id: OperationId::new(),
        input: ModelContent::Text(case.prompt),
        selected_context: None,
        max_steps: case.max_steps,
    };
    let journal = storage.journal();
    let operation = input.operation_id;
    let replay_input = case.replay.then(|| input.clone());
    let result = executor.execute(input, journal.as_ref()).await;
    match (&case.expect.text, &case.expect.error, result) {
        (Some(expected), None, Ok(actual)) => {
            if actual.text != *expected {
                return Err(format!("text: expected {expected:?}, got {:?}", actual.text).into());
            }
            if let Some(expected) = &case.expect.result
                && !matchers::subset(expected, &serde_json::to_value(&actual)?)
            {
                return Err(format!("result: expected {expected}, got {actual:?}").into());
            }
            if let Some(input) = replay_input {
                let requests = provider.requests().len();
                let replay = executor.execute(input, journal.as_ref()).await?;
                if replay != actual || provider.requests().len() != requests {
                    return Err("replay changed the result or dispatched the model again".into());
                }
            }
        }
        (None, Some(expected), Err(actual)) if actual.to_string().contains(expected) => {}
        (_, _, actual) => return Err(format!("unexpected result: {actual:?}").into()),
    }
    check_provider(&case.expect, provider.as_ref())?;
    check_events(&case.expect, journal.as_ref(), operation).await?;
    check_files(case.expect.files, &storage).await
}
fn check_provider(expected: &Expectations, provider: &ScriptedModelProvider) -> TestResult {
    if provider.remaining() != 0 {
        return Err(format!("{} unused scripted turns", provider.remaining()).into());
    }
    if let Some(expected) = &expected.requests {
        let requests = provider
            .requests()
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        matchers::ordered(expected, &requests, "requests")?;
    }
    Ok(())
}
async fn check_events(
    expected: &Expectations,
    journal: &dyn acyclic_harness::executor::ExecutionJournal,
    operation: OperationId,
) -> TestResult {
    if expected.events.is_some() || !expected.not_events.is_empty() {
        let mut events = Vec::new();
        let mut after = 0;
        loop {
            let records = journal.replay(operation, after, 64, None)?.await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                after = record.sequence;
                let mut value = serde_json::to_value(&record.event)?;
                if let ExecutionEvent::Model { event, .. } = &record.event {
                    *value.get_mut("event").ok_or("missing model event")? =
                        serde_json::from_slice(&journal.load(event, None)?.await?)?;
                }
                events.push(value);
            }
        }
        if let Some(expected) = &expected.events {
            matchers::ordered(expected, &events, "events")?;
        }
        for forbidden in &expected.not_events {
            if events
                .iter()
                .any(|event| matchers::subset(forbidden, event))
            {
                return Err(format!("forbidden event: {forbidden}").into());
            }
        }
    }
    Ok(())
}
async fn check_files(
    files: BTreeMap<String, Option<String>>,
    storage: &MemoryHarnessStorage,
) -> TestResult {
    for (path, expected) in files {
        let actual = storage
            .read_delegated_path(storage.owner_scope(), "", &path, None)
            .await;
        match (expected, actual) {
            (Some(expected), Ok((_, actual))) if actual.as_ref() == expected.as_bytes() => {}
            (None, Err(acyclic_harness::Error::NotFound(_))) => {}
            (expected, actual) => {
                return Err(format!("file {path}: expected {expected:?}, got {actual:?}").into());
            }
        }
    }
    Ok(())
}

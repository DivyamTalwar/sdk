use super::{
    TestResult, matchers,
    scenario::{self, Scenario},
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
use std::sync::Arc;

pub(super) async fn run(mut case: Scenario) -> TestResult {
    let limits = case.execution_limits()?;
    let storage = MemoryHarnessStorage::new(AgentId::new(), 1_048_576).await?;
    for (path, contents) in &case.seed {
        storage
            .stage(
                OperationId::new(),
                path,
                contents.as_bytes(),
                "text/plain",
                path.rsplit('/').next().unwrap_or_default(),
            )
            .await?;
    }
    case.seed.clear();
    let (executor, provider) = configure(&mut case, limits)?;
    let input = TurnInput {
        operation_id: OperationId::new(),
        input: ModelContent::Text(std::mem::take(&mut case.prompt)),
        selected_context: None,
        max_steps: case.max_steps,
    };
    let journal = storage.journal();
    let operation = input.operation_id;
    run_turn(&executor, provider.as_ref(), journal.as_ref(), input, &case).await?;
    check_provider(&case.expect, &provider)?;
    check_events(&case.expect, journal.as_ref(), operation).await?;
    check_files(&case.expect, &storage).await
}

fn configure(
    case: &mut Scenario,
    limits: acyclic_harness::conversation::Limits,
) -> acyclic_harness::Result<(StockExecutor, Arc<ScriptedModelProvider>)> {
    let provider = Arc::new(
        ScriptedModelProvider::new(std::mem::take(&mut case.turns))
            .with_reconciliations(std::mem::take(&mut case.recoveries)),
    );
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
    Ok((executor, provider))
}

async fn run_turn(
    executor: &StockExecutor,
    provider: &ScriptedModelProvider,
    journal: &dyn acyclic_harness::executor::ExecutionJournal,
    input: TurnInput,
    case: &Scenario,
) -> TestResult {
    let replay_input = case.replay.then(|| input.clone());
    for expected_error in &case.resume {
        let attempt = (*executor).clone();
        check_interruption(
            expected_error,
            attempt.execute(input.clone(), journal).await,
        )?;
    }
    let result = executor.execute(input, journal).await;
    if let Some(actual) = check_result(&case.expect, result)? {
        check_text(&case.expect, actual.text.as_bytes())?;
        if let Some(input) = replay_input {
            let requests = provider.requests().len();
            let reconciliations = provider.reconciliations().len();
            let replay = executor.execute(input, journal).await?;
            if replay != actual
                || provider.requests().len() != requests
                || provider.reconciliations().len() != reconciliations
            {
                return Err("replay changed the result or dispatched the model again".into());
            }
        }
    }
    Ok(())
}

fn check_provider(
    expected: &scenario::Expectations,
    provider: &ScriptedModelProvider,
) -> TestResult {
    if provider.remaining() != 0 {
        return Err(format!("{} unused scripted turns", provider.remaining()).into());
    }
    let recoveries = provider.recoveries();
    if !recoveries.is_empty() {
        return Err(format!("{} unused reconciliation replies", recoveries.len()).into());
    }
    let requests = provider.requests();
    let reconciliations = provider.reconciliations();
    for (label, expected, actual) in [
        ("requests", expected.request_count, requests.len()),
        (
            "reconciliations",
            expected.reconciliation_count,
            reconciliations.len(),
        ),
    ] {
        if expected.is_some_and(|expected| expected != actual) {
            return Err(format!("{label}: expected {expected:?}, got {actual}").into());
        }
    }
    if let Some(expected) = &expected.requests {
        let requests = requests
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        matchers::ordered(expected, &requests, "requests")?;
    }
    Ok(())
}

async fn check_events(
    expected: &scenario::Expectations,
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
    expected: &scenario::Expectations,
    storage: &MemoryHarnessStorage,
) -> TestResult {
    for (path, expected) in &expected.files {
        let actual = storage
            .read_delegated_path(storage.owner_scope(), "", path, None)
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

fn check_interruption<T: std::fmt::Debug>(
    expected: &str,
    result: acyclic_harness::Result<T>,
) -> TestResult {
    match result {
        Err(error) if error.to_string().contains(expected) => Ok(()),
        actual => Err(format!("expected interrupted attempt {expected:?}, got {actual:?}").into()),
    }
}

fn check_result<T: serde::Serialize + std::fmt::Debug>(
    expected: &scenario::Expectations,
    result: acyclic_harness::Result<T>,
) -> TestResult<Option<T>> {
    match (&expected.error, result) {
        (None, Ok(actual)) => {
            if let Some(expected) = &expected.result
                && !matchers::subset(expected, &serde_json::to_value(&actual)?)
            {
                return Err(format!("result: expected {expected}, got {actual:?}").into());
            }
            Ok(Some(actual))
        }
        (Some(expected), Err(actual)) if actual.to_string().contains(expected) => Ok(None),
        (_, actual) => Err(format!("unexpected result: {actual:?}").into()),
    }
}

fn check_text(expected: &scenario::Expectations, actual: &[u8]) -> TestResult {
    if expected.text.as_deref().map(str::as_bytes) != Some(actual) {
        return Err(format!(
            "text: expected {:?}, got {:?}",
            expected.text,
            String::from_utf8_lossy(actual)
        )
        .into());
    }
    Ok(())
}

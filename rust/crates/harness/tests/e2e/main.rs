//! Data-driven harness scenarios. E2E_FILTER selects a path substring.

mod matchers;
mod scenario;

use acyclic_harness::{
    AgentId, OperationId,
    context::{CompactionPolicy, ContextPipeline},
    conversation::Limits,
    executor::{ExecutionEvent, Executor, StockExecutor, TurnInput},
    filesystem::MemoryHarnessStorage,
    model::{Model, ModelContent},
    test_support::ScriptedModelProvider,
    tool::ToolRegistry,
};
use futures::{StreamExt as _, stream};
use scenario::Scenario;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn discover(root: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            discover(&path, paths)?;
        } else if path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }
    Ok(())
}

async fn run(case: Scenario) -> TestResult {
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
    let provider =
        Arc::new(ScriptedModelProvider::new(case.turns).with_reconciliations(case.recoveries));
    let mut executor = make_executor(provider.clone(), limits)?;
    let input = TurnInput {
        operation_id: OperationId::new(),
        input: ModelContent::Text(case.prompt),
        selected_context: None,
        max_steps: case.max_steps,
    };
    let journal = storage.journal();
    let operation = input.operation_id;
    let replay_input = case.replay.then(|| input.clone());
    for expected_error in case.resume {
        match executor.execute(input.clone(), journal.as_ref()).await {
            Err(error) if error.to_string().contains(&expected_error) => {}
            actual => {
                return Err(format!(
                    "expected interrupted attempt {expected_error:?}, got {actual:?}"
                )
                .into());
            }
        }
        executor = make_executor(provider.clone(), limits)?;
    }
    let result = executor.execute(input, journal.as_ref()).await;
    match (&case.expect.text, &case.expect.error, result) {
        (Some(expected), None, Ok(actual)) => {
            if actual.text != *expected {
                return Err(format!("text: expected {expected:?}, got {:?}", actual.text).into());
            }
            if let Some(expected) = &case.expect.result {
                if !matchers::subset(expected, &serde_json::to_value(&actual)?) {
                    return Err(format!("result: expected {expected}, got {actual:?}").into());
                }
            }
            if let Some(input) = replay_input {
                let requests = provider.request_count();
                let reconciliations = provider.reconciliation_count();
                let replay = executor.execute(input, journal.as_ref()).await?;
                if replay != actual
                    || provider.request_count() != requests
                    || provider.reconciliation_count() != reconciliations
                {
                    return Err("replay changed the result or dispatched the model again".into());
                }
            }
        }
        (None, Some(expected), Err(actual)) if actual.to_string().contains(expected) => {}
        (_, _, actual) => return Err(format!("unexpected result: {actual:?}").into()),
    }
    if provider.remaining() != 0 {
        return Err(format!("{} unused scripted turns", provider.remaining()).into());
    }
    if provider.remaining_reconciliations() != 0 {
        return Err(format!(
            "{} unused reconciliation replies",
            provider.remaining_reconciliations()
        )
        .into());
    }
    for (label, expected, actual) in [
        (
            "requests",
            case.expect.request_count,
            provider.request_count(),
        ),
        (
            "reconciliations",
            case.expect.reconciliation_count,
            provider.reconciliation_count(),
        ),
    ] {
        if expected.is_some_and(|expected| expected != actual) {
            return Err(format!("{label}: expected {expected:?}, got {actual}").into());
        }
    }
    if !case.expect.requests.is_empty() {
        let requests = provider
            .requests()
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        matchers::ordered(&case.expect.requests, &requests, "requests")?;
    }
    if !case.expect.events.is_empty() || !case.expect.not_events.is_empty() {
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
                    value["event"] = serde_json::from_slice(&journal.load(event, None)?.await?)?;
                }
                events.push(value);
            }
        }
        matchers::ordered(&case.expect.events, &events, "events")?;
        for forbidden in &case.expect.not_events {
            if events
                .iter()
                .any(|event| matchers::subset(forbidden, event))
            {
                return Err(format!("forbidden event: {forbidden}").into());
            }
        }
    }
    for (path, expected) in case.expect.files {
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

fn make_executor(
    provider: Arc<ScriptedModelProvider>,
    limits: Limits,
) -> acyclic_harness::Result<StockExecutor> {
    Ok(StockExecutor::new(
        Model::new(
            "scripted".into(),
            "scenario".into(),
            "1".into(),
            Value::Null,
        )?,
        provider,
        ContextPipeline::default(),
        ToolRegistry::default(),
    )
    .with_compaction_policy(CompactionPolicy::Disabled)
    .with_limits(limits))
}

#[tokio::test]
async fn scenario_oracles_reject_wrong_text_context_events_files_and_unused_turns() -> TestResult {
    let baseline = include_bytes!("../scenarios/core/text.json");
    run(Scenario::parse(baseline)?).await?;
    for field in [
        "text",
        "requests",
        "events",
        "not_events",
        "result",
        "files",
        "turns",
        "request_count",
        "reconciliation_count",
        "recoveries",
        "resume",
    ] {
        let mut case = Scenario::parse(baseline)?;
        match field {
            "text" => case.expect.text = Some("wrong".into()),
            "requests" => {
                case.expect.requests = vec![serde_json::json!({"messages":[{"content":"wrong"}]})]
            }
            "events" => case.expect.events = vec![serde_json::json!({"kind":"wrong"})],
            "not_events" => {
                case.expect.not_events = vec![serde_json::json!({"kind":"model_started"})]
            }
            "result" => case.expect.result = Some(serde_json::json!({"steps":99})),
            "files" => {
                case.expect
                    .files
                    .insert("missing.txt".into(), Some("wrong".into()));
            }
            "turns" => case
                .turns
                .push(acyclic_harness::test_support::ScriptedTurn::Text(
                    "unused".into(),
                )),
            "request_count" => case.expect.request_count = Some(0),
            "reconciliation_count" => case.expect.reconciliation_count = Some(1),
            "recoveries" => {
                case.recoveries
                    .push(acyclic_harness::test_support::ScriptedReconciliation {
                        observed: vec![],
                        continuation: None,
                    })
            }
            "resume" => case.resume.push("interrupted".into()),
            _ => return Err("unknown test mutation".into()),
        }
        assert!(run(case).await.is_err(), "accepted incorrect {field}");
    }
    Ok(())
}

#[tokio::test]
async fn recovery_oracles_reject_wrong_prefixes_replies_counts_and_attempts() -> TestResult {
    let baseline = include_bytes!("../scenarios/recovery/midstream.json");
    run(Scenario::parse(baseline)?).await?;
    for field in ["prefix", "continuation", "count", "resume", "unused"] {
        let mut case = Scenario::parse(baseline)?;
        match field {
            "prefix" => case.recoveries[0].observed.clear(),
            "continuation" => case.recoveries[0].continuation = None,
            "count" => case.expect.reconciliation_count = Some(0),
            "resume" => case.resume[0] = "wrong".into(),
            "unused" => case.recoveries.push(case.recoveries[0].clone()),
            _ => return Err("unknown recovery mutation".into()),
        }
        assert!(run(case).await.is_err(), "accepted incorrect {field}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenarios() -> TestResult {
    let started = Instant::now();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios");
    let mut paths = Vec::new();
    discover(&root, &mut paths)?;
    paths.sort();
    if let Ok(filter) = std::env::var("E2E_FILTER") {
        paths.retain(|path| path.to_string_lossy().contains(&filter));
    }
    if paths.is_empty() {
        return Err("no scenarios discovered".into());
    }
    let count = paths.len();
    let mut results = stream::iter(paths.into_iter().map(|path| async move {
        let case_path = path.clone();
        let result: TestResult = tokio::spawn(async move {
            let case = Scenario::parse(&std::fs::read(&case_path)?)?;
            tokio::time::timeout(Duration::from_secs(5), run(case)).await??;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })
        .await
        .unwrap_or_else(|error| Err(Box::new(error)));
        (path, result)
    }))
    .buffer_unordered(16);
    let mut failures = Vec::new();
    while let Some((path, result)) = results.next().await {
        let name = path.strip_prefix(&root)?.display();
        match result {
            Ok(()) => println!("PASS {name}"),
            Err(error) => {
                eprintln!("FAIL {name}: {error}");
                failures.push(path);
            }
        }
    }
    println!(
        "{} scenarios, {} failed, {:.3}s",
        count,
        failures.len(),
        started.elapsed().as_secs_f64()
    );
    if !failures.is_empty() {
        return Err(format!("{} scenarios failed", failures.len()).into());
    }
    if started.elapsed() >= Duration::from_secs(10) {
        return Err("scenario suite exceeded 10 seconds".into());
    }
    Ok(())
}

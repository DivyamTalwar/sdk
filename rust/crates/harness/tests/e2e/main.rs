//! Data-driven harness scenarios. `E2E_FILTER` selects a path substring.
mod execution;
mod matchers;
mod scenario;
use execution::run;
use futures::{StreamExt as _, stream};
use scenario::Scenario;
use std::{
    path::{Path, PathBuf},
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

#[tokio::test]
async fn scenario_oracles_reject_wrong_text_context_events_files_and_unused_turns() -> TestResult {
    let baseline = include_bytes!("../scenarios/core/text.json");
    run(Scenario::parse(baseline)?).await?;
    for field in [
        "text",
        "requests",
        "empty_requests",
        "events",
        "empty_events",
        "not_events",
        "result",
        "files",
        "turns",
    ] {
        let mut case = Scenario::parse(baseline)?;
        match field {
            "text" => case.expect.text = Some("wrong".into()),
            "requests" => {
                case.expect.requests =
                    Some(vec![serde_json::json!({"messages":[{"content":"wrong"}]})]);
            }
            "empty_requests" => case.expect.requests = Some(vec![]),
            "events" => case.expect.events = Some(vec![serde_json::json!({"kind":"wrong"})]),
            "empty_events" => case.expect.events = Some(vec![]),
            "not_events" => {
                case.expect.not_events = vec![serde_json::json!({"kind":"model_started"})];
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
            _ => return Err("unknown test mutation".into()),
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
    Ok(())
}

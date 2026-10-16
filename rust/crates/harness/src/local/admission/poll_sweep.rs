//! Cancel the real run future at each poll, including publication and admission.
use super::*;
use crate::{
    context::CompactionPolicy,
    executor::{ExecutionEvent, ExecutionJournal, TurnOutput},
    model::{ModelDispatch, ModelEvent},
    test_support::{ScriptedModelProvider, ScriptedReconciliation, ScriptedTurn},
};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

const PROMPT: &str = "recover the original run input";

enum Exit {
    Cancelled,
    Completed(TurnOutput),
}

struct CancelAt<F> {
    future: Pin<Box<F>>,
    cut: Option<usize>,
    polls: usize,
}

impl<F: Future<Output = Result<TurnOutput>>> Future for CancelAt<F> {
    type Output = Result<Exit>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.polls += 1;
        if self.cut == Some(self.polls) {
            return Poll::Ready(Ok(Exit::Cancelled));
        }
        self.future
            .as_mut()
            .poll(cx)
            .map(|result| result.map(Exit::Completed))
    }
}

fn events() -> Vec<ModelEvent> {
    vec![
        ModelEvent::Content {
            delta: "done".into(),
        },
        ModelEvent::Completed {
            metadata: serde_json::Value::Null,
        },
    ]
}

fn provider(observed: Vec<ModelEvent>) -> Arc<ScriptedModelProvider> {
    let all = events();
    assert!(
        all.starts_with(&observed),
        "recovery observed another response"
    );
    let continuation = all[observed.len()..].to_vec();
    Arc::new(
        ScriptedModelProvider::new([ScriptedTurn::Text("done".into())]).with_reconciliations([
            ScriptedReconciliation {
                orphan: true,
                observed,
                continuation: Some(continuation),
            },
        ]),
    )
}

async fn model_prefix(harness: &LocalHarness, turn: &Turn) -> Result<Vec<ModelEvent>> {
    let task = crate::runtime::task_id_for_admission(turn.operation);
    if harness.runtime.task_host().outcome(task).await?.is_some() {
        return Ok(vec![]);
    }
    let lease = match harness
        .runtime
        .task_host()
        .recover_work(task, &mut None)
        .await?
    {
        crate::distributed::WorkPull::Claimed(lease) => lease,
        crate::distributed::WorkPull::Idle => return Ok(vec![]),
        crate::distributed::WorkPull::Unresolved { error, .. } => return Err(error),
    };
    let journal = harness
        .runtime
        .execution_journal(
            task,
            crate::scheduler::LeaseFence::from(&lease.reservation),
            turn.model.operation_id,
        )
        .await?;
    let operation = journal
        .execution_operation_id()
        .expect("task-owned execution journal");
    let records = journal
        .replay(
            operation,
            0,
            crate::executor::EXECUTION_REPLAY_PAGE_RECORDS,
            None,
        )?
        .await?;
    assert!(records.len() < crate::executor::EXECUTION_REPLAY_PAGE_RECORDS as usize);
    let mut observed = Vec::new();
    for record in records {
        if let ExecutionEvent::Model { event, .. } = record.event {
            let bytes = journal.load(&event, None)?.await?;
            observed.push(serde_json::from_slice(&bytes).map_err(storage)?);
        }
    }
    Ok(observed)
}

struct Coverage {
    polls: usize,
    cancelled: bool,
    before_head: bool,
    unadmitted: bool,
    admitted: bool,
}

fn assert_dispatches(original: &[ModelDispatch], recovery: &[ModelDispatch]) {
    let first = original
        .first()
        .or_else(|| recovery.first())
        .expect("no model dispatch");
    assert!(original.len() <= 1 && recovery.len() <= 1);
    assert!(
        original
            .iter()
            .chain(recovery)
            .all(|dispatch| dispatch == first),
        "recovery changed the original reservation or request"
    );
}

async fn case(cut: Option<usize>) -> Result<Coverage> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let volume = crate::local::tests::volume(directory.path()).await?;
    let config = |provider: Arc<ScriptedModelProvider>| {
        crate::local::tests::volume_config(directory.path(), volume.clone(), provider).map(
            |config| {
                config
                    .without_default_tools()
                    .compaction(CompactionPolicy::Disabled)
                    .session_limits(SessionLimits {
                        active_tasks: 1,
                        total_tasks: 1,
                        depth: 1,
                        model_steps: 1,
                    })
            },
        )
    };
    let original = provider(vec![]);
    let initial = config(original.clone())?;
    let state = crate::local::owner::state_directory(&initial.project)?;
    let result = Box::pin(async {
        let harness = LocalHarness::open(initial).await?;
        let mut execution = Box::pin(CancelAt {
            future: Box::pin(harness.run(PROMPT)),
            cut,
            polls: 0,
        });
        let exit = (&mut execution).await?;
        let polls = execution.polls;
        let cancelled = matches!(&exit, Exit::Cancelled);
        if let Exit::Completed(output) = &exit {
            assert_eq!(output.text, "done");
        }
        drop(execution);
        drop(harness);

        // Reopen the actual providers before examining the accepted state. A fresh
        // scripted provider recovers exactly its durable model prefix, if any.
        let mut harness = LocalHarness::open(config(provider(vec![]))?).await?;
        let current = harness.current_turn().await?.0;
        let mut coverage = Coverage {
            polls,
            cancelled,
            before_head: current.is_none(),
            unadmitted: false,
            admitted: false,
        };
        let mut observed = vec![];
        if let Some(current) = &current {
            let task = crate::runtime::task_id_for_admission(current.operation);
            let definition = harness
                .runtime
                .harness()
                .task::<serde_json::Value, TurnOutput>("acyclic.stock_turn@1")?;
            match harness.runtime.harness().attach(task, &definition).await {
                Ok(_) => {
                    coverage.admitted = true;
                    observed = model_prefix(&harness, current).await?;
                }
                Err(Error::NotFound(_)) => {
                    // Negative control: main's former direct attach/outcome paths
                    // fail at this real poll cut, rather than a synthetic await.
                    coverage.unadmitted = true;
                    assert!(matches!(
                        harness.runtime.task_host().outcome(task).await,
                        Err(Error::NotFound(_))
                    ));
                    assert!(harness.head_is_terminal(Some(current)).await?);
                }
                Err(error) => return Err(error),
            }
        }
        let recovery = provider(observed);
        harness.provider = recovery.clone();
        let output = if let Some(current) = current {
            let task = crate::runtime::task_id_for_admission(current.operation);
            let output = harness.resume(task).await?;
            assert_eq!(harness.resume(task).await?.text, output.text);
            assert_eq!(harness.current_turn().await?.0, Some(current));
            output
        } else {
            harness.run(PROMPT).await?
        };
        assert_eq!(output.text, "done");
        assert!(
            original.requests().len() + recovery.requests().len() <= 1,
            "the model was generated more than once"
        );
        for request in original.requests().iter().chain(&recovery.requests()) {
            assert!(
                serde_json::to_string(request)
                    .map_err(storage)?
                    .contains(PROMPT),
                "recovery replaced the original input"
            );
        }
        assert_dispatches(
            &original.dispatches(),
            &recovery
                .dispatches()
                .into_iter()
                .chain(recovery.reconciliations())
                .collect::<Vec<_>>(),
        );
        Ok(coverage)
    })
    .await;
    match std::fs::canonicalize(&state) {
        Ok(state) => {
            let root = dirs::data_local_dir()
                .expect("local data directory")
                .join("acyclic/harness")
                .canonicalize()
                .map_err(storage)?;
            assert_eq!(state.parent(), Some(root.as_path()));
            assert!(
                state
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.len() == 64
                        && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
            );
            std::fs::remove_dir_all(state).map_err(storage)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(storage(error)),
    }
    result
}

async fn checked_case(cut: Option<usize>) -> Result<Coverage> {
    Box::pin(case(cut))
        .await
        .map_err(|error| Error::Storage(format!("poll cut {cut:?}: {error:?}")))
}

#[tokio::test]
async fn run_cancellation_at_every_poll_recovers_head_and_admission() -> Result<()> {
    let started = std::time::Instant::now();
    let baseline = Box::pin(checked_case(None)).await?;
    // Every baseline poll is a cut. Independent local providers let their real
    // IO run in parallel; no sleeps, preselected phases, or polling waits.
    let mut coverage = futures::future::try_join_all(
        (1..=baseline.polls).map(|cut| Box::pin(checked_case(Some(cut)))),
    )
    .await?;
    // Continue to the first complete run too: a different accepted IO schedule
    // must not silently truncate the sweep at the baseline's poll count.
    let mut cut = baseline.polls + 1;
    loop {
        let result = Box::pin(checked_case(Some(cut))).await?;
        let cancelled = result.cancelled;
        coverage.push(result);
        if !cancelled {
            break;
        }
        cut += 1;
    }
    assert!(coverage.iter().any(|cut| cut.before_head));
    assert!(
        coverage.iter().any(|cut| cut.unadmitted),
        "no cut split HEAD and admission"
    );
    assert!(coverage.iter().any(|cut| cut.admitted));
    eprintln!(
        "swept {cut} actual run poll cuts in {:?}",
        started.elapsed()
    );
    Ok(())
}

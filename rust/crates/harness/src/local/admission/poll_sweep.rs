//! Cancellation of the real run future at every poll through admission.
use super::*;
use crate::{
    context::CompactionPolicy,
    model::ModelEvent,
    test_support::{ScriptedEvent, ScriptedModelProvider, ScriptedReconciliation, ScriptedTurn},
};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

enum Cut {
    Cancelled,
    ModelReached,
}

struct CancelAt<F> {
    future: Pin<Box<F>>,
    provider: Arc<ScriptedModelProvider>,
    cut: usize,
    polls: usize,
}

impl<F: Future<Output = Result<crate::executor::TurnOutput>>> Future for CancelAt<F> {
    type Output = Result<Cut>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.polls += 1;
        if self.polls == self.cut {
            return Poll::Ready(Ok(Cut::Cancelled));
        }
        match self.future.as_mut().poll(cx) {
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(_)) => Poll::Ready(Err(Error::Conflict(
                "the pending scripted model unexpectedly completed".into(),
            ))),
            Poll::Pending if self.provider.remaining() == 0 => Poll::Ready(Ok(Cut::ModelReached)),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn pending() -> Arc<ScriptedModelProvider> {
    Arc::new(ScriptedModelProvider::new([ScriptedTurn::Events(vec![
        ScriptedEvent::Pending {},
    ])]))
}

fn recovery() -> Arc<ScriptedModelProvider> {
    Arc::new(
        ScriptedModelProvider::new([ScriptedTurn::Text("done".into())]).with_reconciliations([
            ScriptedReconciliation {
                orphan: true,
                observed: vec![],
                continuation: Some(vec![
                    ModelEvent::Content {
                        delta: "done".into(),
                    },
                    ModelEvent::Completed {
                        metadata: serde_json::Value::Null,
                    },
                ]),
            },
        ]),
    )
}

#[tokio::test]
async fn run_cancellation_at_every_poll_recovers_head_and_admission() -> Result<()> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let volume = crate::local::tests::volume(directory.path()).await?;
    let config = |provider: Arc<ScriptedModelProvider>| {
        crate::local::tests::volume_config(directory.path(), volume.clone(), provider).map(
            |config| {
                config
                    .without_default_tools()
                    .compaction(CompactionPolicy::Disabled)
            },
        )
    };
    let mut harness = LocalHarness::open(config(pending())?).await?;
    let started = std::time::Instant::now();
    let mut before_head = 0;
    let mut unadmitted = 0;
    let mut admitted = 0;
    for cut in 1.. {
        let previous = harness.current_turn().await?.0.map(|turn| turn.operation);
        let original = pending();
        harness.provider = original.clone();
        let prompt = format!("recover poll {cut}");
        let mut execution = Box::pin(CancelAt {
            future: Box::pin(harness.run(&prompt)),
            provider: original.clone(),
            cut,
            polls: 0,
        });
        let boundary = (&mut execution).await?;
        let polls = execution.polls;
        drop(execution);
        drop(harness);

        // Opening the same providers recovers any accepted but unacknowledged
        // publication. No sleeps, polling loops, or scheduled timing control a cut.
        let provider = recovery();
        harness = LocalHarness::open(config(provider.clone())?).await?;
        let current = harness.current_turn().await?.0;
        let output = if current.as_ref().map(|turn| turn.operation) == previous {
            before_head += 1;
            harness.run(&prompt).await?
        } else {
            let current = current.expect("a new head disappeared");
            let task = crate::runtime::task_id_for_admission(current.operation);
            let definition = harness
                .runtime
                .harness()
                .task::<serde_json::Value, crate::executor::TurnOutput>("acyclic.stock_turn@1")?;
            match harness.runtime.harness().attach(task, &definition).await {
                Ok(_) => admitted += 1,
                Err(Error::NotFound(_)) => {
                    // Negative control: the old direct attach cannot resume
                    // this exact durable head. Recovery must finish admission.
                    unadmitted += 1;
                    assert!(harness.head_is_terminal(Some(&current)).await?);
                }
                Err(error) => return Err(error),
            }
            let output = harness.resume(task).await?;
            assert_eq!(harness.resume(task).await?.text, output.text);
            assert_eq!(harness.current_turn().await?.0, Some(current));
            output
        };
        assert_eq!(output.text, "done", "cut {cut}");
        let dispatches = original.dispatches();
        let recovered = provider.reconciliations();
        assert!(
            dispatches.len() + provider.requests().len() <= 1,
            "cut {cut}: duplicate model dispatch"
        );
        assert_eq!(
            provider.requests().len() + recovered.len(),
            1,
            "cut {cut}: recovery did not resolve exactly one model request"
        );
        if !dispatches.is_empty() {
            assert_eq!(
                dispatches, recovered,
                "cut {cut}: original dispatch changed"
            );
        }
        if matches!(boundary, Cut::ModelReached) {
            assert!(before_head > 0, "no cancellation preceded HEAD publication");
            assert!(unadmitted > 0, "no cancellation split HEAD and admission");
            assert!(admitted > 0, "no cancellation followed durable admission");
            eprintln!(
                "swept {cut} poll cuts through first model dispatch ({polls} polls); \
                before HEAD={before_head}, unadmitted={unadmitted}, admitted={admitted}; {:?}",
                started.elapsed()
            );
            break;
        }
    }
    Ok(())
}

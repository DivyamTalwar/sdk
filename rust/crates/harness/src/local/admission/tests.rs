use super::*;
use crate::{
    context::CompactionPolicy,
    executor::TurnOutput,
    test_support::{ScriptedModelProvider, ScriptedTurn},
};

async fn fixture() -> Result<(
    tempfile::TempDir,
    LocalHarnessConfig,
    Arc<ScriptedModelProvider>,
)> {
    let directory = tempfile::tempdir().map_err(storage)?;
    let volume = super::super::tests::volume(directory.path()).await?;
    let provider = Arc::new(ScriptedModelProvider::new([
        ScriptedTurn::Text("done".into()),
        ScriptedTurn::Text("done".into()),
        ScriptedTurn::Text("done".into()),
    ]));
    let config = super::super::tests::volume_config(directory.path(), volume, provider.clone())?
        .compaction(CompactionPolicy::Disabled);
    Ok((directory, config, provider))
}

// Interrupt the actual production publication future at each admission await
// boundary: HEAD acknowledged before admission, and admission committed before
// its acknowledgement reaches the caller. A push signal selects the cut; no
// timeout, sleep, background task, or timing assumption controls cancellation.
async fn cancel_at_admission_await(harness: &LocalHarness, after_admission: bool) -> Result<Turn> {
    let operation = OperationId::new();
    let inputs = harness.stage_turn(operation, "original input").await?;
    let (signal, ready) = tokio::sync::oneshot::channel();
    let mut submission = Box::pin(serialized(&harness.admission, async || {
        let previous = harness.current_turn().await?.0.map(|turn| turn.operation);
        harness
            .publish_turn_with(
                operation,
                inputs,
                operation,
                0,
                previous,
                async move |turn| {
                    if after_admission {
                        admitted_task(harness.admit_retained(turn).await?)?;
                    }
                    signal
                        .send(turn.clone())
                        .map_err(|_| Error::Conflict("cancellation observer dropped".into()))?;
                    std::future::pending::<Result<Admission<RuntimeTask<TurnOutput>>>>().await
                },
            )
            .await
    }));
    let retained = tokio::select! {
        _ = &mut submission => return Err(Error::Conflict("submission did not suspend".into())),
        retained = ready => retained.map_err(|_| Error::Conflict("publication did not reach its await boundary".into()))?,
    };
    drop(submission);
    Ok(retained)
}

async fn unadmitted_request_head_does_not_block_a_new_run(
    harness: &LocalHarness,
    provider: &ScriptedModelProvider,
) -> Result<()> {
    let retained = cancel_at_admission_await(harness, false).await?;
    let task = crate::runtime::task_id_for_admission(retained.operation);
    // Negative control: the former unconditional outcome query errors here.
    assert!(matches!(
        harness.runtime.task_host().outcome(task).await,
        Err(Error::NotFound(_))
    ));
    assert!(harness.head_is_terminal(Some(&retained)).await?);
    assert_eq!(harness.run("replacement input").await?.text, "done");
    assert_request(provider, 1, "replacement input");
    assert!(
        harness
            .runtime
            .harness()
            .reconcile_admission(retained.operation)
            .await?
            .is_none(),
        "a replaced orphan request was accidentally admitted"
    );
    Ok(())
}

fn assert_request(provider: &ScriptedModelProvider, count: usize, input: &str) {
    let requests = provider.requests();
    assert_eq!(requests.len(), count, "missing or duplicated model effect");
    assert!(requests[count - 1].messages.iter().any(|message| {
        message.role == crate::model::ModelRole::User
            && matches!(&message.content, crate::model::ModelContent::Text(text) if text == input)
    }), "the model did not receive this case's retained input");
}

#[tokio::test]
async fn cancellation_at_each_head_admission_await_resumes_the_retained_input_once() -> Result<()> {
    let started = std::time::Instant::now();
    // Bootstrap one public composition. Each interrupted case still drops the
    // complete host and reopens its durable state; no recovery cache is shared.
    let (_directory, config, provider) = fixture().await?;
    let model = config.model.clone();
    let project = match &config.project {
        config::LocalProject::Volume { directory, volume } => (directory.clone(), volume.clone()),
        _ => return Err(Error::Invalid("expected volume fixture".into())),
    };
    let mut harness = LocalHarness::open(config).await?;
    let owner = harness.signed.agent();
    eprintln!("admission bootstrap: {:?}", started.elapsed());
    let replacement_started = std::time::Instant::now();
    unadmitted_request_head_does_not_block_a_new_run(&harness, &provider).await?;
    eprintln!("orphan replacement: {:?}", replacement_started.elapsed());
    let mut recovered = Vec::new();
    for after_admission in [false, true] {
        let cut_started = std::time::Instant::now();
        let retained = cancel_at_admission_await(&harness, after_admission).await?;
        let task = crate::runtime::task_id_for_admission(retained.operation);
        let definition = harness
            .runtime
            .harness()
            .task::<serde_json::Value, TurnOutput>("acyclic.stock_turn@1")?;
        let old_attach = harness.runtime.harness().attach(task, &definition).await;
        assert_eq!(
            old_attach.is_ok(),
            after_admission,
            "negative control did not reach the unadmitted-head failure"
        );
        drop(old_attach);
        drop(harness);
        let mut config = crate::test_support::local_harness_config(&project.0, model.clone())?
            .volume(project.1.clone())
            .compaction(CompactionPolicy::Disabled);
        config.goal_continuations = None;
        let open_started = std::time::Instant::now();
        harness = LocalHarness::open(config).await?;
        assert_eq!(harness.signed.agent(), owner, "fixture was rebuilt");
        eprintln!(
            "admission {after_admission} reopen: {:?}",
            open_started.elapsed()
        );
        assert_eq!(harness.resume(task).await?.text, "done");
        assert_eq!(harness.resume(task).await?.text, "done");
        assert_eq!(harness.current_turn().await?.0, Some(retained));
        assert_request(&provider, recovered.len() + 2, "original input");
        recovered.push(after_admission);
        eprintln!(
            "admission {after_admission} cut and recovery: {:?}",
            cut_started.elapsed()
        );
    }
    assert_eq!(recovered, [false, true], "a cancellation cut was omitted");
    assert_eq!(provider.remaining(), 0, "a recovery scenario was omitted");
    eprintln!("admission recovery suite: {:?}", started.elapsed());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    Ok(())
}

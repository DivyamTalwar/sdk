use super::*;

const MAX_FUTURE_BYTES: usize = 4096;

fn assert_bounded<F: std::future::Future>(name: &str, future: F) {
    let bytes = std::mem::size_of_val(&future);
    eprintln!("{name} future: {bytes} bytes");
    assert!(bytes <= MAX_FUTURE_BYTES, "{name} future is {bytes} bytes");
}

#[tokio::test]
async fn execution_and_worker_futures_stay_bounded() -> Result<()> {
    let fixture = Fixture::new().await?;
    let runtime = fixture.runtime(None).await?;
    let lease = fixture.admit(&runtime).await?;
    runtime.task_host().start_task(&lease, &mut None).await?;
    let task = TaskId::from_bytes(lease.operation.operation_id.into_bytes());
    let fence = LeaseFence::from(&lease.reservation);
    let turn = OperationId::from_bytes([106; 16]);
    let model = Model::new("synthetic".into(), "size".into(), "1".into(), Value::Null)?;
    let provider = Arc::new(crate::test_support::ScriptedModelProvider::new([]));
    let execution = runtime
        .stock_execution(
            task,
            fence.clone(),
            turn,
            model.clone(),
            provider.clone(),
            ContextPipeline::default(),
            None,
        )
        .await?;
    let input = TurnInput {
        operation_id: execution.operation_id(),
        input: crate::model::ModelContent::Text("question".into()),
        selected_context: None,
        max_steps: 1,
    };
    assert_bounded(
        "deadline",
        deadline::run(
            runtime.task_host(),
            task,
            Some(&fence),
            Some(20),
            input.operation_id,
            || async { Ok(()) },
            None,
            |_| false,
        ),
    );
    assert_bounded("execution", execution.execute(input.clone()));
    assert_bounded(
        "execution progress",
        execution.execute_progress(input.clone()),
    );
    assert_bounded("model step", execution.model_step(&input, 0, &[]));
    let commands = runtime.commands();
    assert_bounded("run task", runtime.run_task(lease.clone(), &commands, 2));
    let worker_context =
        WorkerContext::Ordinary(Arc::new(runtime.task_context(task, fence.clone()).await?));
    assert_bounded(
        "worker command dispatch",
        runtime.dispatch_commands(task, &worker_context, &fence, &[], &commands, None),
    );
    assert_bounded(
        "run task observed",
        runtime.run_task_observed(&lease, &commands, 2, None),
    );
    assert_bounded(
        "resume task",
        runtime.resume_task(lease.clone(), &commands, 2, None),
    );
    assert_bounded(
        "execution reconciliation",
        execution.reconcile(input.clone(), 1),
    );
    let execution = runtime
        .stock_execution(
            task,
            fence,
            turn,
            model,
            provider,
            ContextPipeline::default(),
            None,
        )
        .await?;
    assert_bounded(
        "execution reconciliation progress",
        execution.reconcile_progress(input),
    );
    Ok(())
}

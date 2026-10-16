use super::*;
use crate::{
    distributed::{WorkPull, Worker},
    runtime::{RuntimeScope, RuntimeTask, TaskSpawner},
    scheduler::LeaseFence,
};

#[tokio::test]
async fn child_agent_keeps_its_admission_and_waits_after_each_received_turn() -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let directory = tempfile::tempdir().map_err(super::super::storage)?;
        let project = volume(directory.path()).await?;
        let provider = scripted([
            ScriptedTurn::Text("first child turn".into()),
            ScriptedTurn::Text("second child turn".into()),
        ]);
        let harness =
            LocalHarness::open(volume_config(directory.path(), project, provider.clone())?).await?;
        let parent_operation = OperationId::new();
        let Admission::Accepted(RuntimeTask::Durable {
            task_id: parent, ..
        }) = harness.submit(parent_operation, "parent").await?
        else {
            panic!("parent admission");
        };
        let worker = Worker {
            id: "child-agent-test".into(),
            available: Default::default(),
            labels: Default::default(),
        };
        let host = harness.runtime.task_host();
        let WorkPull::Claimed(lease) = host
            .pull_work(&worker, Some(parent_operation), &mut None)
            .await?
        else {
            panic!("parent lease");
        };
        host.start_task(&lease, &mut None).await?;
        let fence = LeaseFence::from(&lease.reservation);
        let admitted = host.observe_admission(parent).await?;
        let scope = RuntimeScope::new(admitted.grants, admitted.limits)?
            .with_run_limits(admitted.run_limits, &mut None)?;
        let operation = OperationId::new();
        let (receive, model) = harness
            .stage_turn_with_limits(
                operation,
                "follow the received child request",
                scope.limits(),
            )
            .await?;
        let definition = harness
            .runtime
            .harness()
            .task::<serde_json::Value, serde_json::Value>("acyclic.child_agent@1")?;
        let record = harness
            .runtime
            .harness()
            .registered_admission(
                operation,
                &definition.identity().name,
                &definition.identity().version,
                serde_json::json!({"receive":receive,"model":model}),
                Some((parent, &scope)),
                None,
                None,
            )?
            .0;
        let Admission::Accepted(child) = host.admit_owned(record, fence.clone()).await? else {
            panic!("child admission");
        };
        let publisher = crate::filesystem::FilesystemContentPublisher::new(
            harness.files.clone(),
            &harness.journal,
            &harness.verifier,
            &harness.signed,
            scope.limits().file_bytes,
            None,
        )?;
        use crate::runtime::TaskStateProvider as _;
        for (index, text) in [(1, "first request"), (2, "second request")] {
            let message = OperationId::new();
            let (payload, _) = publisher
                .stage_once(
                    message,
                    &format!("inputs/child-agent/{index}.md"),
                    text.as_bytes(),
                    "text/plain",
                    "request.md",
                    None,
                )?
                .await?;
            host.send_mail(parent, &fence, child, message, payload, None)
                .await?;
            harness.advance(child, &harness.commands()?).await?;
            assert_eq!(provider.requests().len(), index);
            assert!(host.outcome(child).await?.is_none());
            harness.advance(child, &harness.commands()?).await?;
            assert_eq!(provider.requests().len(), index);
        }
        host.cancel(child).await?;
        assert!(matches!(
            host.outcome(child).await?,
            Some(crate::Outcome::Cancelled)
        ));
        host.cancel(parent).await?;
        Ok::<(), crate::Error>(())
    })
    .await
    .map_err(super::super::storage)?
}

#[tokio::test]
async fn child_turn_admission_replays_under_the_parent_lease_and_shared_depth_limit() -> Result<()>
{
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    let project = volume(directory.path()).await?;
    let provider = scripted([]);
    let mut config = volume_config(directory.path(), project, provider.clone())?;
    config.session_limits.depth = 1;
    let harness = LocalHarness::open(config).await?;
    let operation = OperationId::new();
    let Admission::Accepted(RuntimeTask::Durable {
        task_id: parent, ..
    }) = harness.submit(operation, "parent").await?
    else {
        panic!("parent admission was not accepted");
    };
    let worker = Worker {
        id: "child-admission-test".into(),
        available: Default::default(),
        labels: Default::default(),
    };
    let host = harness.runtime.task_host();
    let WorkPull::Claimed(lease) = host.pull_work(&worker, Some(operation), &mut None).await?
    else {
        panic!("parent lease was not claimed");
    };
    host.start_task(&lease, &mut None).await?;
    let fence = LeaseFence::from(&lease.reservation);
    let admitted = host.observe_admission(parent).await?;
    let parent_scope = RuntimeScope::new(admitted.grants, admitted.limits)?
        .with_run_limits(admitted.run_limits, &mut None)?;
    let scope = crate::task_volume::child_scope(&parent_scope, Some(&["read".into()]))?;
    let operation = OperationId::new();
    let Admission::Accepted(RuntimeTask::Durable { task_id: child, .. }) = harness
        .submit_child_turn(operation, "reader", (parent, &scope, &fence))
        .await?
    else {
        panic!("child admission was not accepted");
    };
    assert!(matches!(
        harness.submit_child_turn(operation, "reader", (parent, &scope, &fence)).await?,
        Admission::Accepted(RuntimeTask::Durable { task_id, .. }) if task_id == child
    ));
    let mut stale = fence.clone();
    stale.reservation_id.push_str("-stale");
    assert!(
        harness
            .submit_child_turn(operation, "reader", (parent, &scope, &stale))
            .await
            .is_err()
    );
    let admitted = host.observe_admission(child).await?;
    assert_eq!(admitted.parent, Some(parent));
    assert_eq!(admitted.grants, *scope.grants());
    assert_eq!(admitted.limits, scope.limits());
    assert_eq!(admitted.run_limits, scope.run_limits());
    assert!(admitted.grants.contains("tool:call:read"));
    assert!(!admitted.grants.contains("tool:call:write"));
    let children = harness.runtime.children(parent, None, None, 8).await?;
    assert_eq!(children.entries.len(), 1);
    assert_eq!(children.entries[0].task_id, child);

    let WorkPull::Claimed(lease) = host.pull_work(&worker, Some(operation), &mut None).await?
    else {
        panic!("child lease was not claimed");
    };
    host.start_task(&lease, &mut None).await?;
    let fence = LeaseFence::from(&lease.reservation);
    assert!(matches!(
        harness
            .submit_child_turn(
                OperationId::new(),
                "too deep",
                (child, &scope, &fence)
            )
            .await,
        Err(Error::Invalid(reason)) if reason.contains("depth limit exhausted")
    ));
    assert!(
        harness
            .runtime
            .children(child, None, None, 8)
            .await?
            .entries
            .is_empty()
    );
    assert!(provider.requests().is_empty());
    Ok(())
}

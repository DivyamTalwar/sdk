use super::{
    intake_model::{claim, consumer, publish, suspend},
    session_runtime::{Fixture, VolumeBinding},
    *,
};
use acyclic_harness::{TaskId, task_volume::Component};
use std::sync::Arc;

#[path = "../../src/local/external/recovery.rs"]
mod recovery;

#[test]
fn competing_recoveries_settle_the_retained_slot_before_delivery() {
    model(false);
}

#[test]
#[should_panic(expected = "reply exposed an unfinished workflow input")]
fn negative_control_detects_returning_a_result_without_recovery() {
    model(true);
}

fn model(broken: bool) {
    for (initial, answered) in [(true, true), (false, true), (false, false)] {
        shuttle::check_dfs(
            move || {
                shuttle::future::block_on(Box::pin(async move {
                    let fixture = Arc::new(Fixture::new(VolumeBinding::Separate).await.unwrap());
                    let task = TaskId::from_bytes([81; 16]);
                    let folder = Component::new(task.to_string()).unwrap();
                    let start = request(
                        ExternalHost::Pi,
                        "session_start",
                        json!({"sessionId":"session", "cwd":"/project"}),
                    )
                    .unwrap();
                    let file = publish(&fixture, &folder, &start).await;
                    consumer(&fixture, fixture.publisher.as_ref())
                        .accept(&folder, &file)
                        .await
                        .unwrap();
                    let fence = claim(&fixture, task).await;
                    suspend(&fixture, task, &fence, 2).await;
                    let pending = request(
                        ExternalHost::Pi,
                        "tool_call",
                        json!({
                            "sessionId":"session", "toolCallId":"pending", "toolName":"write",
                            "input":{"path":"note", "content":"pending"}
                        }),
                    )
                    .unwrap();
                    let file = publish(&fixture, &folder, &pending).await;
                    consumer(&fixture, fixture.publisher.as_ref())
                        .accept(&folder, &file)
                        .await
                        .unwrap();
                    let fence = claim(&fixture, task).await;
                    let (revision, _) = fixture
                        .runtime
                        .task_host()
                        .workflow_input(task, &fence, None)
                        .await
                        .unwrap()
                        .unwrap();
                    let mut calls = Vec::new();
                    // A cached attach, a cached tool result, and the next uncached tool
                    // all have to settle the old slot before exposing a reply/admission.
                    for _ in 0..2 {
                        let fixture = fixture.clone();
                        let fence = fence.clone();
                        let revision = revision + 1;
                        calls.push(shuttle::future::spawn(async move {
                            recovery::recover_before_delivery(initial, answered, || async {
                                shuttle::future::yield_now().await;
                                if !broken {
                                    fixture
                                        .runtime
                                        .task_host()
                                        .suspend_workflow(
                                            task,
                                            &fence,
                                            (revision, None),
                                            None,
                                            None,
                                            &mut None,
                                        )
                                        .await?;
                                }
                                Ok(())
                            })
                            .await?;
                            Ok::<_, Error>(())
                        }));
                    }
                    let mut settled = 0;
                    for call in calls {
                        if call.await.unwrap().is_ok() {
                            settled += 1;
                        }
                    }
                    assert!(settled > 0);
                    let next = request(
                        ExternalHost::Pi,
                        "tool_call",
                        json!({
                            "sessionId":"session", "toolCallId":"next", "toolName":"write",
                            "input":{"path":"note", "content":"next"}
                        }),
                    )
                    .unwrap();
                    let file = publish(&fixture, &folder, &next).await;
                    let accepted = consumer(&fixture, fixture.publisher.as_ref())
                        .accept(&folder, &file)
                        .await;
                    assert!(
                        accepted.is_ok(),
                        "reply exposed an unfinished workflow input: {accepted:?}"
                    );
                }));
            },
            None,
        );
    }
}

use super::{*, session_runtime::{Fixture, VolumeBinding}};
use acyclic_harness::{TaskId, task_volume::{Component, FileValue, Query}};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    // Property: a delivered tool request reaches the session slot exactly when
    // its host/session/caller tuple equals the immutable attachment's tuple.
    // A newly admitted task has no suspended slot, so matching requests conflict;
    // a mismatch must be rejected as unauthorized before touching that slot.
    #[test]
    fn attachment_authority_is_the_exact_native_session(
        session in "[a-z]{1,12}", caller in prop::option::of("[a-z]{1,12}"),
        host_changed in any::<bool>(), session_changed in any::<bool>(), caller_changed in any::<bool>(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(Box::pin(async {
            let fixture = Fixture::new(VolumeBinding::Separate).await?;
            let task = Component::new(TaskId::from_bytes([78;16]).to_string()).map_err(storage)?;
            let consumer = RequestConsumer::new(&fixture.runtime, &fixture.requests,
                &fixture.results, fixture.publisher.as_ref(), "/project");
            let original = request(ExternalHost::Pi,"session_start", json!({
                "sessionId":session,"agentId":caller,"cwd":"/project"
            }))?;
            let file = publish(&fixture,&task,&original).await?;
            consumer.accept(&task,&file).await?;
            let host = if host_changed {ExternalHost::ClaudeCode} else {ExternalHost::Pi};
            let session = if session_changed {format!("other-{session}")} else {session};
            let caller = if caller_changed {Some(format!("other-{}",caller.as_deref().unwrap_or("root")))} else {caller};
            let input = if host == ExternalHost::Pi { json!({
                "sessionId":session,"agentId":caller,"toolCallId":"call",
                "toolName":"write","input":{"path":"file","content":"text"}
            }) } else {json!({
                "session_id":session,"agent_id":caller,"tool_use_id":"call",
                "tool_name":"Write","tool_input":{"file_path":"file","content":"text"}
            })};
            let native = request(host, if host == ExternalHost::Pi {"tool_call"} else {"PreToolUse"}, input)?;
            let file = publish(&fixture,&task,&native).await?;
            let answer = consumer.accept(&task,&file).await;
            if host_changed || session_changed || caller_changed {
                assert!(matches!(answer,Err(Error::Unauthorized(_))),"{answer:?}");
            } else {
                assert!(matches!(answer,Err(Error::Conflict(_))),"{answer:?}");
            }
            Ok::<_,Error>(())
        }))?;
    }
}

async fn publish(fixture:&Fixture, task:&Component, native:&ExternalRequest)->Result<FileValue> {
    let file=native.to_file(task.clone()).map_err(storage)?;
    let head=fixture.requests.snapshot(&Query::File(file.key.clone()),1).await.map_err(storage)?;
    fixture.requests.append(head.generation,&file).await.map_err(storage)?;
    Ok(file)
}

fn storage(error:impl std::fmt::Display)->Error {Error::Storage(error.to_string())}

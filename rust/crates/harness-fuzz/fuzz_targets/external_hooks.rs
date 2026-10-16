#![no_main]
//! Native hooks normalize into the same bounded request files as actor clients.

use acyclic_harness::{
    external_executor::{
        CommandSessionMachine, ExternalHost, ExternalRequest, HookBinding, hook_answer, hooks, request,
    },
    task_volume::{Component, FileKey, FileValue, TaskFile},
    workflow::ResumableMachine,
};
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

#[derive(Deserialize)]
struct Hook {
    host: ExternalHost,
    event: String,
    input: serde_json::Value,
}

fn file(
    task: Component,
    request: &ExternalRequest,
    bytes: Vec<u8>,
) -> acyclic_harness::task_volume::Result<FileValue> {
    Ok(FileValue::new(
        FileKey::Task {
            task,
            file: TaskFile::Request(Component::new(request.operation_id.to_string())?),
        },
        bytes,
    ))
}

fn check(input: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    if input.len() > acyclic_stream::MAX_RECORD_BYTES {
        return Ok(());
    }
    let task = Component::new("task")?;
    if let Ok((state, input)) =
        serde_json::from_slice::<(serde_json::Value, serde_json::Value)>(input)
    {
        let machine = CommandSessionMachine::new();
        if let Ok(initial) = machine.initialize(&input) {
            let emitted = machine.transition(&initial, &serde_json::Value::Null)?;
            assert_eq!(emitted.commands.len(), 1);
        }
        if let Ok(next) = machine.transition(&state, &input) {
            assert!(next.commands.len() <= 1);
            assert_eq!(machine.transition(&state, &input)?, next);
            let bytes = serde_json::to_vec(&next.state)?;
            let decoded: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert_eq!(decoded, next.state);
        }
    }
    for host in [
        ExternalHost::Codex,
        ExternalHost::ClaudeCode,
        ExternalHost::Pi,
    ] {
        for spec in hooks(host) {
            if let Ok(answer) = hook_answer(host, spec.event, input) {
                let encoded = serde_json::to_vec(&answer)?;
                assert_eq!(hook_answer(host, spec.event, &encoded)?, answer);
                assert!(answer.is_object());
                assert!(answer.get("Failed").is_none());
            }
        }
    }
    if let Ok(binding) = HookBinding::from_bytes(input) {
        let encoded = serde_json::to_vec(&binding)?;
        let decoded = HookBinding::from_bytes(&encoded)?;
        assert_eq!(decoded.requests, binding.requests);
        assert_eq!(decoded.results, binding.results);
        assert_ne!(decoded.requests, decoded.results);
    }
    if let Ok(path) = std::str::from_utf8(input)
        && let Ok(key) = FileKey::parse(path)
    {
        assert_eq!(FileKey::parse(&key.path())?, key);
    }
    if let Ok(hook) = serde_json::from_slice::<Hook>(input)
        && let Ok(request) = request(hook.host, &hook.event, hook.input)
    {
        if let Ok(call)=request.tool_call(&acyclic_harness::tools::defaults()?) {
            assert!(!call.revision.is_empty());
        }
        match request.to_file(task.clone()) {
            Ok(file) => assert_eq!(ExternalRequest::from_file(&task, &file)?, request),
            Err(acyclic_harness::task_volume::Error::LimitExceeded) => {}
            Err(error) => return Err(error.into()),
        }
    }
    if let Ok(request) = serde_json::from_slice::<ExternalRequest>(input) {
        if let Ok(call) = request.tool_call(&acyclic_harness::tools::defaults()?) {
            let bytes=serde_json::to_vec(&call)?;
            assert_eq!(serde_json::from_slice::<acyclic_harness::filesystem::ToolTaskCommand>(&bytes)?,call);
        }
        let original = file(task.clone(), &request, input.to_vec())?;
        if let Ok(accepted) = ExternalRequest::from_file(&task, &original) {
            assert_eq!(accepted, request);
            let bytes = serde_json::to_vec(&accepted)?;
            assert_eq!(
                ExternalRequest::from_file(&task, &file(task.clone(), &accepted, bytes)?)?,
                accepted
            );
            assert!(ExternalRequest::from_file(&Component::new("other-task")?, &original).is_err());
        }
    }
    Ok(())
}

fuzz_target!(|input: &[u8]| {
    let result = check(input);
    assert!(result.is_ok(), "hook request file invariant: {result:?}");
});

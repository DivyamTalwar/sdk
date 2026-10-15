#![no_main]
//! Native hooks normalize into the same bounded request files as actor clients.

use acyclic_harness::{
    external_executor::{ExternalHost, ExternalRequest, request},
    task_volume::{Component, FileKey, FileValue, TaskFile},
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
    if let Ok(hook) = serde_json::from_slice::<Hook>(input)
        && let Ok(request) = request(hook.host, &hook.event, hook.input)
    {
        let bytes = serde_json::to_vec(&request)?;
        if bytes.len() <= acyclic_stream::MAX_RECORD_BYTES {
            assert_eq!(
                ExternalRequest::from_file(&task, &file(task.clone(), &request, bytes)?)?,
                request
            );
        }
    }
    if let Ok(request) = serde_json::from_slice::<ExternalRequest>(input) {
        let original = file(task.clone(), &request, input.to_vec())?;
        if let Ok(accepted) = ExternalRequest::from_file(&task, &original) {
            assert_eq!(accepted, request);
            let bytes = serde_json::to_vec(&accepted)?;
            assert_eq!(
                ExternalRequest::from_file(&task, &file(task.clone(), &accepted, bytes)?)?,
                accepted
            );
            assert!(
                ExternalRequest::from_file(&Component::new("other-task")?, &original).is_err()
            );
        }
    }
    Ok(())
}

fuzz_target!(|input: &[u8]| {
    let result = check(input);
    assert!(result.is_ok(), "hook request file invariant: {result:?}");
});

use super::ExternalRequest;
use crate::{
    Result,
    task_volume::{Component, FileValue},
};
use serde_json::Value;

/// Dispatches one authenticated request file through the runtime's effect binding.
/// The binding retains the original operation identity and owns task admission,
/// tool approval/execution and child fork/join through the existing workflow host.
/// Publish the returned immutable file with that host's checkpoint and fence.
pub fn handle<'a, F, Fut>(
    task: &'a Component,
    file: &'a FileValue,
    apply: F,
) -> impl Future<Output = Result<FileValue>> + 'a
where
    F: FnOnce(ExternalRequest) -> Fut + 'a,
    Fut: Future<Output = Result<Value>> + 'a,
{
    Box::pin(async move {
        let request = ExternalRequest::from_file(task, file)
            .map_err(|error| crate::Error::Invalid(error.to_string()))?;
        let key = request
            .result_key(task.clone())
            .map_err(|error| crate::Error::Invalid(error.to_string()))?;
        let answer = apply(request).await?;
        let bytes = super::volume::encode(&answer)
            .map_err(|error| crate::Error::Invalid(error.to_string()))?;
        Ok(FileValue::new(key, bytes))
    })
}

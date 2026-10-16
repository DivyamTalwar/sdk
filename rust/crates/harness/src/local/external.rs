//! Native request files share the existing task runtime and checkout publisher.
use super::*;
use crate::{
    external_executor::{ExternalAction, ExternalRequest, RequestCommands, RequestConsumer},
    task_volume::{Component, Query, TaskVolume},
};
use acyclic_stream::StreamPath;
use serde_json::Value;

mod recovery;
#[cfg(all(test, not(acyclic_harness_loom)))]
mod tests;

pub(super) struct Volumes {
    requests: TaskVolume<LocalStream>,
    results: TaskVolume<LocalStream>,
}

impl Volumes {
    pub(super) fn open(stream: Arc<LocalStream>) -> impl Future<Output = Result<Self>> {
        Box::pin(async move {
            let requests = TaskVolume::open(
                stream.clone(),
                StreamPath::new("external/requests").map_err(storage)?,
            )
            .await
            .map_err(storage)?;
            let results = TaskVolume::open(
                stream,
                StreamPath::new("external/results").map_err(storage)?,
            )
            .await
            .map_err(storage)?;
            Ok(Self { requests, results })
        })
    }
}

impl LocalHarness {
    fn external_volumes(&self) -> (&TaskVolume<LocalStream>, &TaskVolume<LocalStream>) {
        (&self.external_volumes.requests, &self.external_volumes.results)
    }

    /// Reads an existing native result without admitting or executing work.
    #[cfg(feature = "hooks")]
    pub fn external_tool_result<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> impl Future<Output = Result<Value>> + 'a {
        Box::pin(async move {
            let (requests, results) = self.external_volumes();
            crate::external_executor::read_native_result(name, arguments, requests, results).await
        })
    }

    /// Publishes and drives one external request through the sole task owner.
    /// Identical retries read the immutable result without repeating an effect.
    pub fn external_request<'a>(
        &'a self,
        request: &'a ExternalRequest,
    ) -> impl Future<Output = Result<Value>> + 'a {
        Box::pin(admission::serialized(&self.admission, async move || {
            #[cfg(feature = "native-execution")]
            if let ExternalAction::Attach { workspace } = &request.action {
                if let Some(path) = self.workspace_path() {
                    if std::fs::canonicalize(workspace).map_err(storage)? != path {
                        return Err(Error::Unauthorized(
                            "native workspace differs from the selected checkout".into(),
                        ));
                    }
                }
            }
            let (requests, results) = self.external_volumes();
            let task_id = request.task_id();
            let task = Component::new(task_id.to_string()).map_err(storage)?;
            let file = request.to_file(task.clone()).map_err(storage)?;
            crate::external_executor::publish_request(requests, &file, None, None)
                .await
                .map_err(storage)?;
            let key = request.result_key(task.clone()).map_err(storage)?;
            let cached = results
                .snapshot(&Query::File(key), 1)
                .await
                .map_err(storage)?;
            let commands = RequestCommands::new(&self.runtime, requests, results);
            recovery::recover_before_delivery(
                matches!(request.action, ExternalAction::Attach { .. }),
                !cached.files.is_empty(),
                || Box::pin(self.advance(task_id, &commands)),
            )
            .await?;
            if let Some(file) = cached.files.first() {
                return crate::external_executor::volume::decode(&file.bytes).map_err(storage);
            }
            let workspace = match &request.action {
                ExternalAction::Attach { workspace } => workspace.as_str(),
                _ => "",
            };
            let publisher = FilesystemContentPublisher::new(
                self.files.clone(),
                &self.journal,
                &self.verifier,
                &self.signed,
                self.limits.file_bytes,
                None,
            )?;
            RequestConsumer::new(
                &self.runtime,
                requests,
                results,
                publisher.as_ref(),
                workspace,
            )
            .accept(&task, &file)
            .await?;
            Box::pin(self.advance(task_id, &commands)).await?;
            request.follow_result(results, task).await.map_err(storage)
        }))
    }
}

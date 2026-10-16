//! Native request files share the existing task runtime and checkout publisher.
use super::*;
use crate::{
    external_executor::{ExternalAction, ExternalRequest, RequestCommands, RequestConsumer},
    task_volume::{Component, Query, TaskVolume},
};
use acyclic_stream::StreamPath;
use serde_json::Value;

impl LocalHarness {
    fn external_volumes(
        &self,
    ) -> impl Future<Output = Result<(TaskVolume<LocalStream>, TaskVolume<LocalStream>)>> + '_ {
        Box::pin(async move {
            let stream = self.runtime.task_host().workflow_stream();
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
            Ok((requests, results))
        })
    }

    /// Reads an existing native result without admitting or executing work.
    #[cfg(feature = "hooks")]
    pub fn external_tool_result<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> impl Future<Output = Result<Value>> + 'a {
        Box::pin(async move {
            let (requests, results) = self.external_volumes().await?;
            crate::external_executor::read_native_result(name, arguments, &requests, &results).await
        })
    }

    /// Publishes and drives one external request through the sole task owner.
    /// Identical retries read the immutable result without repeating an effect.
    pub fn external_request<'a>(
        &'a self,
        request: &'a ExternalRequest,
    ) -> impl Future<Output = Result<Value>> + 'a {
        Box::pin(async move {
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
            let (requests, results) = self.external_volumes().await?;
            let task_id = request.task_id();
            let task = Component::new(task_id.to_string()).map_err(storage)?;
            let file = request.to_file(task.clone()).map_err(storage)?;
            let snapshot = requests
                .snapshot(&Query::File(file.key.clone()), 1)
                .await
                .map_err(storage)?;
            match snapshot.files.first() {
                Some(original) if original != &file => {
                    return Err(Error::Conflict("native request bytes changed".into()));
                }
                Some(_) => {}
                None => {
                    requests
                        .append(snapshot.generation, &file)
                        .await
                        .map_err(storage)?;
                }
            }
            let key = request.result_key(task.clone()).map_err(storage)?;
            let cached = results
                .snapshot(&Query::File(key), 1)
                .await
                .map_err(storage)?;
            if let Some(file) = cached.files.first() {
                return crate::external_executor::volume::decode(&file.bytes).map_err(storage);
            }
            let workspace = match &request.action {
                ExternalAction::Attach { workspace } => workspace.as_str(),
                _ => "",
            };
            let publisher = FilesystemContentPublisher::new(
                self.files.clone(),
                &self.scratch,
                &self.verifier,
                &self.signed,
                self.limits.file_bytes,
                None,
            )?;
            RequestConsumer::new(
                &self.runtime,
                &requests,
                &results,
                publisher.as_ref(),
                workspace,
            )
            .accept(&task, &file)
            .await?;
            let commands = RequestCommands::new(&self.runtime, &requests, &results);
            Box::pin(self.advance(task_id, &commands)).await?;
            request.follow_result(&results, task).await.map_err(storage)
        })
    }
}

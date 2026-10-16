//! Native process entry point; task state stays in provider-backed files.
use super::{ExternalHost, request, volume};
use crate::{
    Error, Result,
    task_volume::{Component, TaskVolume},
};
use acyclic_stream::{StreamPath, http::HttpStream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Read as _, path::PathBuf, sync::Arc};

/// Platform bindings read from the user's configuration volume.
/// The token may append only to `requests`, and may only read `results`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookBinding {
    /// Streams endpoint; HTTPS or loopback HTTP, as validated by the provider.
    pub endpoint: String,
    /// Credential file, outside task requests and hook output.
    pub bearer_file: PathBuf,
    /// Caller-writable request-file volume.
    #[serde(
        serialize_with = "serialize_path",
        deserialize_with = "deserialize_path"
    )]
    pub requests: StreamPath,
    /// Actor-writable, caller-readable result-file volume.
    #[serde(
        serialize_with = "serialize_path",
        deserialize_with = "deserialize_path"
    )]
    pub results: StreamPath,
    /// Actor/task selected by the caller's platform binding.
    pub task: Component,
}

fn serialize_path<S: serde::Serializer>(
    path: &StreamPath,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(path.as_str())
}

fn deserialize_path<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<StreamPath, D::Error> {
    StreamPath::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

impl HookBinding {
    /// Decodes a bounded configuration document; no transport is contacted.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let binding: Self = volume::decode(bytes).map_err(storage)?;
        if binding.requests == binding.results {
            return Err(Error::Invalid(
                "hook requests and results require separate owner paths".into(),
            ));
        }
        Ok(binding)
    }
}

/// Reads one native event and uses only the platform Streams provider.
pub async fn run_hook(host: &str, event: &str) -> Result<Value> {
    let host: ExternalHost = serde_json::from_value(Value::String(host.into())).map_err(storage)?;
    let mut input: Value = volume::decode(&read_bounded(
        std::io::stdin().lock(),
        acyclic_stream::MAX_RECORD_BYTES,
    )?)
    .map_err(storage)?;
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(storage)?);
    let mcp = super::handoff::normalize_tool(host, event, &mut input)?;
    let request = request(host, event, input)?;
    if matches!(request.action, super::ExternalAction::RunTool { .. })
        && host != ExternalHost::Pi
        && !mcp
    {
        return Err(Error::Unsupported(
            "Use the Acyclic MCP coding tools so execution and results share the task fence."
                .into(),
        ));
    }
    let (answer, task) = run_request(&request, &cwd).await?;
    if mcp {
        // The ordinary MCP call returns this already-published result. Its
        // rewritten arguments identify the exact original immutable request.
        super::handoff::answer(&request, &cwd, task, &answer)
    } else if host == ExternalHost::Pi
        && matches!(request.action, super::ExternalAction::RunTool { .. })
    {
        super::tool_result(answer)
    } else {
        super::answer::validate(host, event, answer)
    }
}

pub(super) fn run_request<'a>(
    request: &'a super::ExternalRequest,
    cwd: &'a std::path::Path,
) -> impl Future<Output = Result<(Value, Component)>> + 'a {
    Box::pin(async move {
        let Some(path) = std::env::var_os("ACYCLIC_HARNESS_BINDING") else {
            #[cfg(all(feature = "filesystem-local", feature = "native-execution"))]
            {
                let harness =
                    crate::LocalHarness::open(crate::LocalHarnessConfig::external(cwd)).await?;
                return Ok((
                    harness.external_request(request).await?,
                    Component::new(request.task_id().to_string()).map_err(storage)?,
                ));
            }
            #[cfg(not(all(feature = "filesystem-local", feature = "native-execution")))]
            return Err(Error::Unsupported(
                "local hooks require filesystem-local and native-execution".into(),
            ));
        };
        let (requests, results, task) = platform_volumes(path).await?;
        Ok((
            request
                .send(&requests, &results, task.clone())
                .await
                .map_err(storage)?,
            task,
        ))
    })
}

pub(super) fn read_tool_result(
    name: &str,
    arguments: Value,
) -> impl Future<Output = Result<Value>> + '_ {
    Box::pin(async move {
        let Some(path) = std::env::var_os("ACYCLIC_HARNESS_BINDING") else {
            #[cfg(all(feature = "filesystem-local", feature = "native-execution"))]
            {
                let cwd = super::handoff::cwd(&arguments)?;
                let harness =
                    crate::LocalHarness::open(crate::LocalHarnessConfig::external(cwd)).await?;
                return harness.external_tool_result(name, arguments).await;
            }
            #[cfg(not(all(feature = "filesystem-local", feature = "native-execution")))]
            return Err(Error::Unsupported(
                "local hooks require filesystem-local and native-execution".into(),
            ));
        };
        let (requests, results, task) = platform_volumes(path).await?;
        let key = arguments
            .pointer("/_acyclic/request")
            .cloned()
            .ok_or_else(|| Error::Unauthorized("missing native request file".into()))?;
        let key: crate::task_volume::FileKey = serde_json::from_value(key).map_err(storage)?;
        if !matches!(&key,crate::task_volume::FileKey::Task {task:bound,..} if bound == &task) {
            return Err(Error::Unauthorized(
                "native result belongs to another bound task".into(),
            ));
        }
        super::handoff::read_result(name, arguments, &requests, &results).await
    })
}

async fn platform_volumes(
    path: std::ffi::OsString,
) -> Result<(TaskVolume<HttpStream>, TaskVolume<HttpStream>, Component)> {
    let binding = HookBinding::from_bytes(&read_bounded(
        std::fs::File::open(path).map_err(storage)?,
        acyclic_stream::MAX_RECORD_BYTES,
    )?)?;
    let token = read_bounded(
        std::fs::File::open(&binding.bearer_file).map_err(storage)?,
        acyclic_stream::MAX_BEARER_TOKEN_BYTES,
    )?;
    let token = std::str::from_utf8(&token).map_err(storage)?.trim();
    let provider = Arc::new(
        HttpStream::new(
            &binding.endpoint,
            token,
            acyclic_stream::MAX_RECORD_BYTES * 4,
        )
        .map_err(storage)?,
    );
    let requests = TaskVolume::open(provider.clone(), binding.requests)
        .await
        .map_err(storage)?;
    let results = TaskVolume::open(provider, binding.results)
        .await
        .map_err(storage)?;
    Ok((requests, results, binding.task))
}

fn read_bounded(reader: impl std::io::Read, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() > maximum {
        return Err(Error::Invalid("hook input exceeds its record bound".into()));
    }
    Ok(bytes)
}

fn storage(error: impl std::fmt::Display) -> Error {
    Error::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_hook_and_request_futures_stay_below_sixteen_kib() -> Result<()> {
        let request = request(
            ExternalHost::Pi,
            "session_start",
            serde_json::json!({"sessionId":"size","cwd":"project"}),
        )?;
        let hook = run_hook("pi", "session_start");
        let call = run_request(&request, std::path::Path::new("project"));
        let result = read_tool_result("bash", Value::Null);
        eprintln!(
            "native hook/request futures: {}/{} bytes",
            std::mem::size_of_val(&hook),
            std::mem::size_of_val(&call)
        );
        assert!(std::mem::size_of_val(&hook) < 16 * 1024);
        assert!(std::mem::size_of_val(&call) < 16 * 1024);
        assert!(std::mem::size_of_val(&result) < 16 * 1024);
        Ok(())
    }
}

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
    let input: Value = volume::decode(&read_bounded(
        std::io::stdin().lock(),
        acyclic_stream::MAX_RECORD_BYTES,
    )?)
    .map_err(storage)?;
    let request = request(host, event, input)?;
    let path = std::env::var_os("ACYCLIC_HARNESS_BINDING").ok_or_else(|| {
        Error::Invalid("ACYCLIC_HARNESS_BINDING must name a platform binding file".into())
    })?;
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
    request
        .send(&requests, &results, binding.task)
        .await
        .map_err(storage)
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

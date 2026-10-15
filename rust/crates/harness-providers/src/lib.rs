//! Shared streaming adapters and immutable model selection.

mod credentials;
mod http;
mod projection;
mod registry;
mod sse;
mod stream;
#[cfg(test)]
mod tests;

pub use credentials::Credentials;
pub use http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
pub use registry::{Adapter, ProviderConfig, Registry, ResolvedModel};

use acyclic_harness::{Error, Result};
use serde_json::Value;

fn invalid(reason: impl Into<String>) -> Error {
    Error::Invalid(reason.into())
}

fn json(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))
}

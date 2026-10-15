//! Shared streaming adapters and immutable model selection.

mod accounting;
#[cfg(not(target_arch = "wasm32"))]
mod codex;
mod credentials;
mod dispatch;
#[cfg(not(target_arch = "wasm32"))]
pub use codex::CodexLogin;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod codex_dispatch_tests;
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

fn http_error(status: u16) -> Error {
    let message = format!("model HTTP status {status}");
    match status {
        401 | 403 => Error::Unauthorized(message),
        408 | 429 | 500..=599 => Error::Storage(message),
        _ => Error::Invalid(message),
    }
}

fn json(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))
}

//! Shared streaming adapters and immutable model selection.

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
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

pub use credentials::Credentials;
#[cfg(target_arch = "wasm32")]
pub use http::BrowserTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use http::ReqwestTransport;
pub use http::{HttpRequest, HttpResponse, HttpTransport};
pub use registry::{Adapter, ProviderConfig, Registry, ResolvedModel};

use acyclic_harness::{Error, Result};
use serde_json::Value;

fn invalid(reason: impl Into<String>) -> Error {
    Error::Invalid(reason.into())
}

fn json(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))
}

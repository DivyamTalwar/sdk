//! Shared streaming adapters and immutable model selection.

mod accounting;
mod adapter;
mod catalog;
#[cfg(not(target_arch = "wasm32"))]
mod codex;
#[cfg(not(target_arch = "wasm32"))]
pub use codex::CodexLogin;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod codex_dispatch_tests;
mod credentials;
mod dispatch;
mod http;
#[cfg(not(target_arch = "wasm32"))]
mod native;
mod projection;
mod registry;
mod resolved;
mod spec;
mod sse;
mod stream;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

pub use adapter::Adapter;
pub use catalog::default_catalog;
pub use credentials::Credentials;
pub use http::{HttpRequest, HttpResponse, HttpTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use http::ReqwestTransport;
#[cfg(target_arch = "wasm32")]
pub use http::BrowserTransport;
pub use registry::Registry;
pub use resolved::ResolvedModel;
pub use spec::{Authentication, ProviderSpec};

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

use acyclic_harness::Result;
use acyclic_stream::{BoxProviderFuture, BoxProviderStream, ProviderPlatform};
use bytes::Bytes;
use std::collections::BTreeMap;

/// One streaming JSON request. Headers may contain credentials; deliberately not Debug.
pub struct HttpRequest {
    /// Fully resolved endpoint.
    pub url: String,
    /// Protocol and authentication headers.
    pub headers: BTreeMap<String, String>,
    /// Exact encoded JSON.
    pub body: Vec<u8>,
}

/// Response whose body remains lazy until polled.
pub struct HttpResponse {
    /// HTTP status.
    pub status: u16,
    /// Incremental body. Dropping it cancels I/O.
    pub body: BoxProviderStream<'static, Result<Bytes>>,
}

/// Platform HTTP boundary, also used by scripted fixtures.
pub trait HttpTransport: ProviderPlatform {
    /// Sends exactly one request without retries.
    fn send(&self, request: HttpRequest) -> BoxProviderFuture<'_, Result<HttpResponse>>;
}

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::ReqwestTransport;
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::BrowserTransport;

pub(crate) fn platform() -> Result<std::sync::Arc<dyn HttpTransport>> {
    #[cfg(not(target_arch = "wasm32"))]
    let transport = ReqwestTransport::new()?;
    #[cfg(target_arch = "wasm32")]
    let transport = BrowserTransport::global()?;
    Ok(std::sync::Arc::new(transport))
}

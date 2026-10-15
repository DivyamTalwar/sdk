use acyclic_harness::Result;
use acyclic_stream::{BoxProviderFuture, BoxProviderStream, ProviderPlatform};
use bytes::Bytes;
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
mod native;

/// One streaming JSON request. Headers may contain credentials; deliberately not Debug.
pub struct HttpRequest {
    /// Fully resolved endpoint.
    pub url: String,
    /// Protocol and authentication headers.
    pub headers: BTreeMap<String, String>,
    /// Exact encoded JSON.
    pub body: Bytes,
}

/// Response whose body remains lazy until polled.
pub struct HttpResponse {
    /// HTTP status.
    pub status: u16,
    /// Incremental body. Dropping it cancels I/O.
    pub body: BoxProviderStream<'static, Result<Bytes>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpResponse {
    /// Admits the concrete body stream before boxing it on the issued ledger.
    pub fn with_native_body<S>(
        status: u16,
        stream: S,
        work: &acyclic_fs::NativeMaterializationWork,
    ) -> Result<Self>
    where
        S: futures::Stream<Item = Result<Bytes>> + Send + Unpin + 'static,
    {
        Ok(Self {
            status,
            body: native::body(stream, Some(work))?,
        })
    }
}

/// Platform HTTP boundary, also used by scripted fixtures.
pub trait HttpTransport: ProviderPlatform {
    /// Sends exactly one request without retries.
    fn send(&self, request: HttpRequest) -> BoxProviderFuture<'_, Result<HttpResponse>>;
    /// Whether this transport funds its native callback frames before allocation.
    #[cfg(not(target_arch = "wasm32"))]
    fn supports_native(&self, work: &acyclic_fs::NativeMaterializationWork) -> Result<bool> {
        work.remaining()?;
        Ok(false)
    }
    /// Issues one HTTP exchange on the original invocation ledger.
    #[cfg(not(target_arch = "wasm32"))]
    fn send_with_native_work<'a>(
        &'a self,
        _: HttpRequest,
        work: &'a acyclic_fs::NativeMaterializationWork,
    ) -> Result<acyclic_harness::NativeFutureBundle<'a, HttpResponse>> {
        work.remaining()?;
        Err(acyclic_harness::Error::NativeBoundary {
            code: acyclic_harness::wire::ErrorCode::Unsupported,
            reason: "HTTP transport has no native producer",
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod reqwest;
#[cfg(not(target_arch = "wasm32"))]
pub use reqwest::ReqwestTransport;
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

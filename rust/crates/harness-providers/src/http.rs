use acyclic_harness::Result;
use acyclic_stream::{BoxProviderFuture, BoxProviderStream, ProviderPlatform};
use bytes::Bytes;
use futures::StreamExt as _;
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

/// Pooled native HTTP; reqwest uses browser fetch on wasm32.
pub struct ReqwestTransport(reqwest::Client);

impl ReqwestTransport {
    /// Creates the platform transport. Credentials are never forwarded across redirects.
    pub fn new() -> Result<Self> {
        let builder = reqwest::Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        Ok(Self(builder.build().map_err(|_| {
            super::invalid("HTTP client initialization failed")
        })?))
    }
}

impl HttpTransport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> BoxProviderFuture<'_, Result<HttpResponse>> {
        Box::pin(async move {
            let mut builder = self
                .0
                .post(request.url)
                .header("content-type", "application/json");
            for (name, value) in request.headers {
                builder = builder.header(name, value);
            }
            let response = builder.body(request.body).send().await.map_err(|_| {
                acyclic_harness::Error::Storage("model HTTP exchange failed".into())
            })?;
            Ok(HttpResponse {
                status: response.status().as_u16(),
                body: Box::pin(response.bytes_stream().map(|chunk| {
                    chunk.map_err(|_| {
                        acyclic_harness::Error::Storage("model HTTP body interrupted".into())
                    })
                })),
            })
        })
    }
}

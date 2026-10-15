use super::{HttpRequest, HttpResponse, HttpTransport};
use crate::invalid;
use acyclic_harness::Result;
use acyclic_stream::BoxProviderFuture;
use futures::StreamExt as _;

/// Pooled native HTTP with redirects and retries disabled.
pub struct ReqwestTransport(reqwest::Client);

impl ReqwestTransport {
    /// Creates the platform transport. Credentials are never forwarded across redirects.
    pub fn new() -> Result<Self> {
        let builder = reqwest::Client::builder();
        let builder = builder
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        Ok(Self(builder.build().map_err(|_| {
            invalid("HTTP client initialization failed")
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
            let response = builder
                .body(request.body)
                .send()
                .await
                .map_err(|_| invalid("model HTTP exchange failed"))?;
            Ok(HttpResponse {
                status: response.status().as_u16(),
                body: Box::pin(
                    response
                        .bytes_stream()
                        .map(|chunk| chunk.map_err(|_| invalid("model HTTP body interrupted"))),
                ),
            })
        })
    }
}

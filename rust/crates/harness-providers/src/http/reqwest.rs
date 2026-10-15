use super::*;
use futures::StreamExt as _;

/// Pooled native HTTP with redirects and implicit retries disabled.
pub struct ReqwestTransport(::reqwest::Client);

impl ReqwestTransport {
    async fn exchange(
        &self,
        request: HttpRequest,
        #[cfg(not(target_arch = "wasm32"))] work: Option<&acyclic_fs::NativeMaterializationWork>,
    ) -> Result<HttpResponse> {
        #[cfg(not(target_arch = "wasm32"))]
        let native = work.is_some();
        #[cfg(target_arch = "wasm32")]
        let native = false;
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
            .map_err(|_| transport_error(native, "model HTTP exchange failed"))?;
        let status = response.status().as_u16();
        let body = response.bytes_stream().map(move |chunk| {
            chunk.map_err(|_| transport_error(native, "model HTTP body interrupted"))
        });
        #[cfg(not(target_arch = "wasm32"))]
        let body = native::body(body, work)?;
        #[cfg(target_arch = "wasm32")]
        let body = Box::pin(body);
        Ok(HttpResponse { status, body })
    }
    /// Creates the platform transport. Credentials are never forwarded across redirects.
    pub fn new() -> Result<Self> {
        let builder = ::reqwest::Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder
            .redirect(::reqwest::redirect::Policy::none())
            .retry(::reqwest::retry::never());
        Ok(Self(builder.build().map_err(|_| {
            crate::invalid("HTTP client initialization failed")
        })?))
    }
}

fn transport_error(native: bool, reason: &'static str) -> acyclic_harness::Error {
    #[cfg(not(target_arch = "wasm32"))]
    if native {
        return acyclic_harness::Error::NativeBoundary {
            code: acyclic_harness::wire::ErrorCode::Storage,
            reason,
        };
    }
    let _ = native;
    acyclic_harness::Error::Storage(reason.into())
}

impl HttpTransport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> BoxProviderFuture<'_, Result<HttpResponse>> {
        Box::pin(self.exchange(
            request,
            #[cfg(not(target_arch = "wasm32"))]
            None,
        ))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn supports_native(&self, work: &acyclic_fs::NativeMaterializationWork) -> Result<bool> {
        work.remaining()?;
        Ok(true)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn send_with_native_work<'a>(
        &'a self,
        request: HttpRequest,
        work: &'a acyclic_fs::NativeMaterializationWork,
    ) -> Result<acyclic_harness::NativeFutureBundle<'a, HttpResponse>> {
        acyclic_harness::NativeFutureBundle::new(work, || self.exchange(request, Some(work)))
    }
}

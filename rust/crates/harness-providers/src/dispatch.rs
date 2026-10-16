use crate::{Adapter, HttpRequest, HttpResponse, ResolvedModel, invalid, projection};
use acyclic_harness::{
    Result,
    model::{ModelDispatch, ModelEvent, PreparedModelRequest},
};
use acyclic_stream::BoxProviderStream;
use futures::StreamExt as _;
use std::{borrow::Cow, collections::BTreeMap};

pub(crate) fn generate(
    provider: &ResolvedModel,
    request: PreparedModelRequest,
    dispatch: ModelDispatch,
) -> BoxProviderStream<'_, Result<ModelEvent>> {
    Box::pin(async_stream::try_stream! {
        provider.validate(&request.request().model)?;
        if request.request_digest() != dispatch.request_digest {
            Err(invalid("model dispatch digest differs from prepared request"))?;
        }
        let names = crate::tool_names::Names::new(request.request());
        let auth = authenticate(provider, #[cfg(not(target_arch = "wasm32"))] None).await?;
        let (response, input_bytes) = send(provider, &request, &names, dispatch, auth.0.as_deref(), auth.1.as_deref()).await?;
        #[cfg(not(target_arch = "wasm32"))]
        let (response, input_bytes) = if response.status == 401 {
            if let Some(login) = &provider.codex_login {
                // No stream bytes have been read; a rejected credential did not start inference.
                drop(response);
                let (key, account) = login.bearer(provider.transport.as_ref(), auth.0.as_deref(), None).await?;
                send(provider, &request, &names, dispatch, Some(&key), account.as_deref()).await?
            } else { (response, input_bytes) }
        } else { (response, input_bytes) };
        drop(auth);
        if !(200..300).contains(&response.status) {
            Err(crate::http_error(response.status))?;
        }
        let mut events = crate::stream::events_named(provider.config.adapter, response.body, provider.config.max_event_bytes, dispatch, Some(&names));
        while let Some(event) = events.next().await {
            let event = event?;
            if let ModelEvent::Completed { metadata } = &event { provider.estimator.observe(input_bytes, metadata); }
            yield event;
        }
    })
}

pub(crate) async fn authenticate<'a>(
    provider: &'a ResolvedModel,
    #[cfg(not(target_arch = "wasm32"))] custody: Option<&mut acyclic_fs::NativeAllocation>,
) -> Result<(Option<Cow<'a, str>>, Option<String>)> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(login) = &provider.codex_login {
        let (key, account) = login
            .bearer(provider.transport.as_ref(), None, custody)
            .await?;
        return Ok((Some(Cow::Owned(key)), account));
    }
    Ok((provider.key.as_deref().map(Cow::Borrowed), None))
}

pub(crate) async fn send(
    provider: &ResolvedModel,
    request: &PreparedModelRequest,
    names: &crate::tool_names::Names<'_>,
    dispatch: ModelDispatch,
    key: Option<&str>,
    account: Option<&str>,
) -> Result<(HttpResponse, usize)> {
    let (url, headers, body) = self::request(provider, request, names, dispatch, key, account)?;
    let input_bytes = body.len();
    let response = provider
        .transport
        .send(HttpRequest {
            url,
            headers,
            body: body.into(),
        })
        .await?;
    Ok((response, input_bytes))
}

pub(crate) fn request(
    provider: &ResolvedModel,
    request: &PreparedModelRequest,
    names: &crate::tool_names::Names<'_>,
    dispatch: ModelDispatch,
    key: Option<&str>,
    account: Option<&str>,
) -> Result<(String, BTreeMap<String, String>, Vec<u8>)> {
    let body = projection::body_named(provider, request.request(), names)?;
    let mut headers = BTreeMap::from([("accept".into(), "text/event-stream".into())]);
    if let Some(key) = key {
        let (name, value) = match provider.config.adapter {
            Adapter::Anthropic => ("x-api-key", key.into()),
            Adapter::Gemini => ("x-goog-api-key", key.into()),
            _ => ("authorization", format!("Bearer {key}")),
        };
        headers.insert(name.into(), value);
    }
    if provider.config.adapter == Adapter::Anthropic {
        headers.insert("anthropic-version".into(), "2023-06-01".into());
        #[cfg(target_arch = "wasm32")]
        headers.insert(
            "anthropic-dangerous-direct-browser-access".into(),
            "true".into(),
        );
    }
    if let Some(account) = account {
        headers.insert("chatgpt-account-id".into(), account.into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    if provider.codex_login.is_some() {
        headers.insert("originator".into(), "acyclic_harness".into());
        headers.insert(
            "user-agent".into(),
            concat!("acyclic-harness/", env!("CARGO_PKG_VERSION")).into(),
        );
        headers.insert("session-id".into(), dispatch.operation_id.to_string());
        headers.insert("thread-id".into(), dispatch.operation_id.to_string());
        headers.insert(
            "x-client-request-id".into(),
            format!("{}-{}", dispatch.operation_id, dispatch.step),
        );
    }
    let _ = dispatch;
    let encoded = serde_json::to_vec(&body).map_err(|error| invalid(error.to_string()))?;
    Ok((projection::endpoint(provider)?, headers, encoded))
}

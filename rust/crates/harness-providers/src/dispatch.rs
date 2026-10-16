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
        let auth = authenticate(provider).await?;
        let (response, input_bytes) = send(provider, &request, &names, dispatch, auth.0.as_deref(), auth.1.as_deref()).await?;
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

pub(crate) async fn authenticate(
    provider: &ResolvedModel,
) -> Result<(Option<Cow<'_, str>>, Option<String>)> {
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
    let images = crate::images::load(provider, request.request(), None).await?;
    let (url, headers, body) = self::request(
        provider,
        request,
        names,
        dispatch,
        key,
        account,
        images.iter().map(|image| image.0.as_ref()),
    )?;
    // Image usage must not calibrate the text-only bytes/token estimator.
    let input_bytes = if images.is_empty() { body.len() } else { 0 };
    drop(images);
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

pub(crate) fn request<'a>(
    provider: &ResolvedModel,
    request: &PreparedModelRequest,
    names: &crate::tool_names::Names<'_>,
    dispatch: ModelDispatch,
    key: Option<&str>,
    account: Option<&str>,
    images: impl Iterator<Item = &'a [u8]>,
) -> Result<(String, BTreeMap<String, String>, Vec<u8>)> {
    let body = projection::body_with_images(provider, request.request(), names, images, false)?;
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
    let _ = dispatch;
    let encoded = serde_json::to_vec(&body).map_err(|error| invalid(error.to_string()))?;
    Ok((projection::endpoint(provider)?, headers, encoded))
}

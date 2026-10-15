use crate::{Adapter, HttpTransport, ProviderSpec, invalid, projection};
use acyclic_harness::{
    Result,
    context::{ModelContextCapacity, ModelTokenCount},
    model::{Model, ModelDispatch, ModelEvent, ModelProvider, PreparedModelRequest},
};
use acyclic_stream::BoxProviderStream;
use std::{collections::BTreeMap, sync::Arc};

/// Exact model selection and its provider, ready for a harness execution scope.
pub struct ResolvedModel {
    pub(crate) estimator: crate::accounting::TokenEstimator,
    /// Immutable harness model identity.
    pub(crate) model: Model,
    pub(crate) config: Arc<ProviderSpec>,
    pub(crate) capacity: ModelContextCapacity,
    pub(crate) key: Option<String>,
    pub(crate) transport: Arc<dyn HttpTransport>,
}

impl ResolvedModel {
    /// Borrows the immutable identity used to bind this provider into the harness.
    pub fn model(&self) -> &Model {
        &self.model
    }
    pub(crate) fn context_output(&self) -> u32 {
        self.capacity.output_tokens
    }
    pub(crate) fn validate(&self, model: &Model) -> Result<()> {
        if model != &self.model {
            return Err(invalid("request model differs from resolved selection"));
        }
        Ok(())
    }
}

impl ModelProvider for ResolvedModel {
    fn context_capacity(&self, model: &Model) -> Result<ModelContextCapacity> {
        self.validate(model)?;
        Ok(self.capacity)
    }

    fn count_tokens(&self, request: &PreparedModelRequest) -> Result<ModelTokenCount> {
        self.validate(&request.request().model)?;
        projection::count(self, request)
    }

    fn generate<'a>(
        &'a self,
        request: PreparedModelRequest,
        dispatch: ModelDispatch,
    ) -> BoxProviderStream<'a, Result<ModelEvent>> {
        Box::pin(async_stream::try_stream! {
            self.validate(&request.request().model)?;
            if request.request_digest() != dispatch.request_digest {
                Err(invalid("model dispatch digest differs from prepared request"))?;
            }
            let body = projection::body(self, request.request())?;
            let mut headers = BTreeMap::new();
            headers.insert("accept".into(), "text/event-stream".into());
            if let Some(key) = &self.key {
                let (name, value) = match self.config.adapter {
                    Adapter::Anthropic => ("x-api-key", key.clone()),
                    Adapter::Gemini => ("x-goog-api-key", key.clone()),
                    _ => ("authorization", format!("Bearer {key}")),
                };
                headers.insert(name.into(), value);
            }
            if self.config.adapter == Adapter::Anthropic {
                headers.insert("anthropic-version".into(), "2023-06-01".into());
                #[cfg(target_arch = "wasm32")]
                headers.insert("anthropic-dangerous-direct-browser-access".into(), "true".into());
            }
            let body = serde_json::to_vec(&body).map_err(|error| invalid(error.to_string()))?;
            let input_bytes = body.len();
            let response = self.transport.send(crate::HttpRequest {
                url: projection::endpoint(self)?, headers,
                body,
            }).await?;
            if !(200..300).contains(&response.status) {
                Err(crate::http_error(response.status))?;
            }
            use futures::StreamExt as _;
            let mut events = crate::stream::events(self.config.adapter, response.body, self.config.max_event_bytes, dispatch);
            while let Some(event) = events.next().await {
                let event = event?;
                if let ModelEvent::Completed { metadata } = &event {
                    self.estimator.observe(input_bytes, metadata);
                }
                yield event;
            }
        })
    }
}

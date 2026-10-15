use crate::{HttpTransport, ProviderSpec, invalid, projection};
use acyclic_harness::{
    Result,
    context::{ModelContextCapacity, ModelTokenCount},
    model::{Model, ModelDispatch, ModelEvent, ModelProvider, PreparedModelRequest},
};
use acyclic_stream::BoxProviderStream;
use std::sync::Arc;

/// Exact model selection and its provider, ready for a harness execution scope.
pub struct ResolvedModel {
    pub(crate) estimator: crate::accounting::TokenEstimator,
    /// Immutable harness model identity.
    pub(crate) model: Model,
    pub(crate) config: Arc<ProviderSpec>,
    pub(crate) capacity: ModelContextCapacity,
    pub(crate) key: Option<String>,
    pub(crate) transport: Arc<dyn HttpTransport>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) codex_login: Option<Arc<crate::CodexLogin>>,
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
        crate::dispatch::generate(self, request, dispatch)
    }
}

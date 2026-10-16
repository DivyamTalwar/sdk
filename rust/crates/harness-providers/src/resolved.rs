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
    pub(crate) content: Option<Arc<dyn acyclic_harness::conversation::ContentResidencyVerifier>>,
}

impl ResolvedModel {
    /// Binds the caller-authorized filesystem reader; canonical requests retain file references.
    pub fn with_content_reader(
        mut self,
        reader: Arc<dyn acyclic_harness::conversation::ContentResidencyVerifier>,
    ) -> Self {
        self.content = Some(reader);
        self
    }

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
    #[cfg(not(target_arch = "wasm32"))]
    fn supports_native_model(&self, work: &acyclic_fs::NativeMaterializationWork) -> Result<bool> {
        work.remaining()?;
        self.transport.supports_native(work)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn context_capacity_with_native_work(
        &self,
        model: &Model,
        work: &acyclic_fs::NativeMaterializationWork,
    ) -> Result<ModelContextCapacity> {
        work.remaining()?;
        if model != &self.model {
            return Err(acyclic_harness::Error::NativeBoundary {
                code: acyclic_harness::wire::ErrorCode::Invalid,
                reason: "request model differs from resolved selection",
            });
        }
        Ok(self.capacity)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn count_tokens_with_native_work(
        &self,
        request: &PreparedModelRequest,
        work: &acyclic_fs::NativeMaterializationWork,
    ) -> Result<(ModelTokenCount, acyclic_fs::NativeAllocation)> {
        crate::native::count(self, request, work)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn generate_with_native_work<'a>(
        &'a self,
        request: &'a PreparedModelRequest,
        dispatch: ModelDispatch,
        work: &'a acyclic_fs::NativeMaterializationWork,
    ) -> Result<
        acyclic_harness::NativeFutureBundle<'a, acyclic_harness::model::NativeModelStream<'a>>,
    > {
        crate::native::generate(self, request, dispatch, work)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn reconcile_with_native_work<'a>(
        &'a self,
        _: ModelDispatch,
        _: &'a [ModelEvent],
        work: &'a acyclic_fs::NativeMaterializationWork,
    ) -> Result<
        acyclic_harness::NativeFutureBundle<
            'a,
            (Option<Vec<ModelEvent>>, acyclic_fs::NativeAllocation),
        >,
    > {
        acyclic_harness::NativeFutureBundle::new(work, || async move {
            Ok((None, work.claim(0, 0, acyclic_fs::WorkCounters::UNCHARGED)?))
        })
    }

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

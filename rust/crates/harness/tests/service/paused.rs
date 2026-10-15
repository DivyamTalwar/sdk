use super::harness::{
    Result,
    context::{ModelContextCapacity, ModelTokenCount},
    model::{Model, ModelDispatch, ModelEvent, ModelProvider, PreparedModelRequest},
    test_support::{ScriptedModelProvider, ScriptedTurn},
};
use acyclic_stream::BoxProviderStream;
use futures::StreamExt as _;
use tokio::sync::Notify;

pub(super) struct PausedModel {
    pub started: Notify,
    pub script: ScriptedModelProvider,
}

impl PausedModel {
    pub fn new() -> Self {
        Self {
            started: Notify::new(),
            script: ScriptedModelProvider::new([ScriptedTurn::Text("done".into())]),
        }
    }
}

impl ModelProvider for PausedModel {
    fn context_capacity(&self, model: &Model) -> Result<ModelContextCapacity> {
        self.script.context_capacity(model)
    }
    fn count_tokens(&self, request: &PreparedModelRequest) -> Result<ModelTokenCount> {
        self.script.count_tokens(request)
    }
    fn generate<'a>(
        &'a self,
        request: PreparedModelRequest,
        dispatch: ModelDispatch,
    ) -> BoxProviderStream<'a, Result<ModelEvent>> {
        let response = self.script.generate(request, dispatch);
        let gate = futures::stream::once(async move {
            self.started.notify_one();
            std::future::pending::<()>().await;
            response
        });
        Box::pin(gate.flatten())
    }
}

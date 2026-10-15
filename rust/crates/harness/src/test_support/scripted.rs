use crate::{
    Error, Result,
    context::{ModelContextCapacity, ModelTokenCount},
    model::{Model, ModelDispatch, ModelEvent, ModelProvider, ModelRequest, PreparedModelRequest},
};
use acyclic_stream::BoxProviderStream;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::Mutex};

/// One provider response. A string emits text followed by completion; an event
/// list is replayed exactly, including errors, incomplete streams and metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScriptedTurn {
    /// Complete text-only response.
    Text(String),
    /// Exact ordered streaming response.
    Events(Vec<ScriptedEvent>),
}

/// One event or failure in an exact streaming response.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ScriptedEvent {
    /// Provider-neutral stream event.
    Event(ModelEvent),
    /// Failure at this position in the stream.
    Error {
        /// Diagnostic returned as a provider storage error.
        error: String,
    },
}

#[derive(Default)]
struct State {
    turns: VecDeque<ScriptedTurn>,
    requests: Vec<PreparedModelRequest>,
    dispatches: Vec<ModelDispatch>,
}

/// Replays responses once and retains the exact requests delivered by the executor.
/// Exhaustion fails instead of silently repeating the final response.
pub struct ScriptedModelProvider {
    state: Mutex<State>,
}

impl ScriptedModelProvider {
    /// Constructs an independent script; sharing the provider shares its cursor.
    #[must_use]
    pub fn new(turns: impl IntoIterator<Item = ScriptedTurn>) -> Self {
        Self {
            state: Mutex::new(State {
                turns: turns.into_iter().collect(),
                ..State::default()
            }),
        }
    }

    /// Snapshots complete requests, including system messages, tools and context.
    pub fn requests(&self) -> Result<Vec<ModelRequest>> {
        Ok(self
            .lock()?
            .requests
            .iter()
            .map(|r| r.request().clone())
            .collect())
    }

    /// Snapshots dispatch identities in the same order as the recorded requests.
    pub fn dispatches(&self) -> Result<Vec<ModelDispatch>> {
        Ok(self.lock()?.dispatches.clone())
    }

    /// Number of responses not yet requested.
    pub fn remaining(&self) -> Result<usize> {
        Ok(self.lock()?.turns.len())
    }

    /// Number of recorded requests without copying their context.
    pub fn request_count(&self) -> Result<usize> {
        Ok(self.lock()?.requests.len())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| Error::Storage("scripted model lock poisoned".into()))
    }

    fn next(&self, request: PreparedModelRequest, dispatch: ModelDispatch) -> Result<ScriptedTurn> {
        if dispatch.request_digest != request.request_digest() {
            return Err(Error::Invalid(
                "scripted model request digest mismatch".into(),
            ));
        }
        let mut state = self.lock()?;
        state.requests.push(request);
        state.dispatches.push(dispatch);
        state
            .turns
            .pop_front()
            .ok_or_else(|| Error::Invalid("scripted model exhausted".into()))
    }
}

impl ModelProvider for ScriptedModelProvider {
    fn context_capacity(&self, _: &Model) -> Result<ModelContextCapacity> {
        Ok(ModelContextCapacity {
            context_tokens: 131_072,
            output_tokens: 4_096,
        })
    }

    fn count_tokens(&self, request: &PreparedModelRequest) -> Result<ModelTokenCount> {
        Ok(request.serialized_token_upper_bound(None)?.0)
    }

    fn generate<'a>(
        &'a self,
        request: PreparedModelRequest,
        dispatch: ModelDispatch,
    ) -> BoxProviderStream<'a, Result<ModelEvent>> {
        match self.next(request, dispatch) {
            Ok(ScriptedTurn::Text(delta)) => Box::pin(futures::stream::iter([
                Ok(ModelEvent::Content { delta }),
                Ok(ModelEvent::Completed {
                    metadata: serde_json::Value::Null,
                }),
            ])),
            Ok(ScriptedTurn::Events(events)) => Box::pin(futures::stream::iter(
                events.into_iter().map(|event| match event {
                    ScriptedEvent::Event(event) => Ok(event),
                    ScriptedEvent::Error { error } => Err(Error::Storage(error)),
                }),
            )),
            Err(error) => Box::pin(futures::stream::iter([Err(error)])),
        }
    }
}

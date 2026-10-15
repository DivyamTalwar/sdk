use crate::{Credentials, HttpTransport, ReqwestTransport, invalid, projection};
use acyclic_harness::{
    Result,
    context::{ModelContextCapacity, ModelTokenCount},
    model::{Model, ModelDispatch, ModelEvent, ModelProvider, PreparedModelRequest},
};
use acyclic_stream::BoxProviderStream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// Wire protocol implemented by an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    /// Anthropic Messages with API keys only.
    Anthropic,
    /// OpenAI Responses.
    Responses,
    /// OpenAI-compatible Chat Completions.
    Chat,
    /// Gemini generateContent.
    Gemini,
}

/// Endpoint configuration independent of immutable model identity.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Protocol dialect.
    pub adapter: Adapter,
    /// HTTPS API root, or loopback HTTP API root.
    pub base_url: String,
    /// Exact model capacities, including local server configurations.
    #[serde(default)]
    pub models: BTreeMap<String, ModelContextCapacity>,
    /// Maximum bytes in one SSE event and one accumulated tool call.
    pub max_event_bytes: usize,
}

/// Registry of protocol endpoints addressed by `provider/model`.
pub struct Registry {
    providers: BTreeMap<String, ProviderConfig>,
    credentials: Credentials,
    transport: Arc<dyn HttpTransport>,
    #[cfg(not(target_arch = "wasm32"))]
    codex_login: Option<Arc<crate::CodexLogin>>,
}

impl Registry {
    /// Standard endpoints; unknown capacities must be registered explicitly.
    pub fn new(credentials: Credentials) -> Result<Self> {
        Ok(Self::with_transport(
            credentials,
            Arc::new(ReqwestTransport::new()?),
        ))
    }

    /// Uses the same adapters with a caller-owned transport.
    pub fn with_transport(credentials: Credentials, transport: Arc<dyn HttpTransport>) -> Self {
        let mut providers = BTreeMap::new();
        for (name, adapter, base) in [
            (
                "anthropic",
                Adapter::Anthropic,
                "https://api.anthropic.com/v1",
            ),
            ("openai", Adapter::Responses, "https://api.openai.com/v1"),
            ("openrouter", Adapter::Chat, "https://openrouter.ai/api/v1"),
            ("ollama", Adapter::Chat, "http://localhost:11434/v1"),
            ("lmstudio", Adapter::Chat, "http://localhost:1234/v1"),
            ("vllm", Adapter::Chat, "http://localhost:8000/v1"),
            (
                "gemini",
                Adapter::Gemini,
                "https://generativelanguage.googleapis.com/v1beta",
            ),
        ] {
            providers.insert(
                name.into(),
                ProviderConfig {
                    adapter,
                    base_url: base.into(),
                    models: BTreeMap::new(),
                    max_event_bytes: 1024 * 1024,
                },
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        let codex_login = credentials
            .allows_native_sources()
            .then(crate::CodexLogin::discover)
            .flatten()
            .map(Arc::new);
        Self {
            providers,
            credentials,
            transport,
            #[cfg(not(target_arch = "wasm32"))]
            codex_login,
        }
    }

    /// Supplies a specific native Codex CLI login, including for scripted refresh tests.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_codex_login(mut self, login: crate::CodexLogin) -> Self {
        self.codex_login = Some(Arc::new(login));
        self
    }

    /// Adds or replaces one endpoint. Local models require their configured capacities.
    pub fn register(&mut self, provider: String, config: ProviderConfig) -> Result<()> {
        if provider.trim().is_empty() || provider.contains('/') || config.max_event_bytes == 0 {
            return Err(invalid("invalid provider registration"));
        }
        validate_url(&config.base_url)?;
        for capacity in config.models.values() {
            capacity.validate()?;
        }
        self.providers.insert(provider, config);
        Ok(())
    }

    /// Resolves a model without doing network I/O. Remaining slashes belong to the model.
    pub fn resolve(&self, selection: &str) -> Result<ResolvedModel> {
        let (provider, name) = selection
            .split_once('/')
            .ok_or_else(|| invalid("expected provider/model"))?;
        let config = self
            .providers
            .get(provider)
            .ok_or_else(|| invalid("unknown model provider"))?
            .clone();
        let capacity = config
            .models
            .get(name)
            .copied()
            .or_else(|| capacity(provider, name))
            .ok_or_else(|| {
                invalid("register the exact selected model context and output capacities")
            })?;
        let model = Model::new(
            provider.into(),
            name.into(),
            "provider-v1".into(),
            json!({}),
        )?;
        let key = self.credentials.api_key(provider)?;
        #[cfg(not(target_arch = "wasm32"))]
        let codex_login = if provider == "openai" && key.is_none() {
            self.codex_login
                .as_ref()
                .map(|login| Arc::new(login.fresh()))
        } else {
            None
        };
        #[cfg(not(target_arch = "wasm32"))]
        let config = if codex_login.is_some() {
            if config.adapter != Adapter::Responses
                || config.base_url != "https://api.openai.com/v1"
            {
                return Err(invalid(
                    "Codex CLI credentials require the built-in Responses route",
                ));
            }
            ProviderConfig {
                base_url: "https://chatgpt.com/backend-api/codex".into(),
                ..config
            }
        } else {
            config
        };
        let authenticated = key.is_some();
        #[cfg(not(target_arch = "wasm32"))]
        let authenticated = authenticated || codex_login.is_some();
        if !authenticated && !matches!(provider, "ollama" | "lmstudio" | "vllm") {
            return Err(invalid("model provider API key is unavailable"));
        }
        Ok(ResolvedModel {
            estimator: crate::accounting::TokenEstimator::new(config.adapter),
            model,
            config,
            capacity,
            key,
            transport: self.transport.clone(),
            #[cfg(not(target_arch = "wasm32"))]
            codex_login,
        })
    }
}

/// Exact model selection and its provider, ready for a harness execution scope.
pub struct ResolvedModel {
    pub(crate) estimator: crate::accounting::TokenEstimator,
    /// Immutable harness model identity.
    pub(crate) model: Model,
    pub(crate) config: ProviderConfig,
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

pub(crate) fn validate_url(base: &str) -> Result<url::Url> {
    let url = url::Url::parse(base).map_err(|_| invalid("invalid provider URL"))?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "provider URL requires HTTPS or loopback HTTP without userinfo, query or fragment",
        ));
    }
    Ok(url)
}

fn capacity(provider: &str, name: &str) -> Option<ModelContextCapacity> {
    let (context_tokens, output_tokens) = match (provider, name) {
        ("anthropic", "claude-sonnet-4-5" | "claude-sonnet-4-5-20250929") => (200_000, 64_000),
        ("gemini", "gemini-2.5-pro" | "gemini-2.5-flash") => (1_048_576, 65_536),
        ("openai", "gpt-4.1" | "gpt-4.1-2025-04-14") => (1_047_576, 32_768),
        ("openai", "gpt-5" | "gpt-5-2025-08-07") => (400_000, 128_000),
        _ => return None,
    };
    Some(ModelContextCapacity {
        context_tokens,
        output_tokens,
    })
}

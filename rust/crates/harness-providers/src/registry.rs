use crate::{
    Authentication, Credentials, HttpTransport, ProviderSpec, ReqwestTransport, ResolvedModel,
    invalid,
};
use acyclic_harness::{Result, model::Model};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// Registry of protocol endpoints addressed by `provider/model`.
pub struct Registry {
    providers: BTreeMap<String, Arc<ProviderSpec>>,
    credentials: Credentials,
    transport: Arc<dyn HttpTransport>,
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
        let providers = crate::catalog::default_catalog()
            .into_iter()
            .map(|(id, spec)| (id, Arc::new(spec)))
            .collect();
        Self {
            providers,
            credentials,
            transport,
        }
    }

    /// Borrows declarative endpoint, authentication and capacity specifications.
    pub fn catalog(&self) -> &BTreeMap<String, Arc<ProviderSpec>> {
        &self.providers
    }

    /// Adds or replaces one endpoint. Local models require their configured capacities.
    pub fn register(&mut self, provider: String, config: ProviderSpec) -> Result<()> {
        if provider.trim().is_empty() || provider.contains('/') || config.max_event_bytes == 0 {
            return Err(invalid("invalid provider registration"));
        }
        validate_url(&config.base_url)?;
        for capacity in config.models.values() {
            capacity.validate()?;
        }
        self.providers.insert(provider, Arc::new(config));
        Ok(())
    }

    /// Resolves a model without doing network I/O. Remaining slashes belong to the model.
    pub fn resolve(&self, selection: &str) -> Result<ResolvedModel> {
        self.resolve_with_options(selection, json!({}))
    }

    /// Binds options into the resolved immutable model identity before dispatch.
    pub fn resolve_with_options(
        &self,
        selection: &str,
        options: serde_json::Value,
    ) -> Result<ResolvedModel> {
        if !options.is_object() {
            return Err(invalid("model options must be an object"));
        }
        let (provider, name) = selection
            .split_once('/')
            .ok_or_else(|| invalid("expected provider/model"))?;
        let config = self
            .providers
            .get(provider)
            .ok_or_else(|| invalid("unknown model provider"))?
            .clone();
        let capacity = config.models.get(name).copied().ok_or_else(|| {
            invalid("register the exact selected model context and output capacities")
        })?;
        let model = Model::new(provider.into(), name.into(), "provider-v1".into(), options)?;
        let key = self.credentials.api_key(provider)?;
        if key.is_none() && config.authentication == Authentication::ApiKey {
            return Err(invalid("model provider API key is unavailable"));
        }
        Ok(ResolvedModel {
            estimator: crate::accounting::TokenEstimator::new(config.adapter),
            model,
            config,
            capacity,
            key,
            transport: self.transport.clone(),
        })
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

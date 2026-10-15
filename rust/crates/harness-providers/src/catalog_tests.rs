use super::*;

#[tokio::test]
async fn arbitrary_provider_ids_follow_their_catalog_authentication_policy() -> Result<()> {
    let (_, script) = provider(Adapter::Chat, 200);
    let mut registry =
        Registry::with_transport(Credentials::explicit(BTreeMap::new()), script.clone());
    let mut spec = ProviderSpec {
        adapter: Adapter::Chat,
        authentication: Authentication::Optional,
        base_url: "http://127.0.0.1:1234/v1".into(),
        max_event_bytes: 4096,
        models: BTreeMap::from([(
            "m".into(),
            ModelContextCapacity {
                context_tokens: 8192,
                output_tokens: 1024,
            },
        )]),
    };
    registry.register("local".into(), spec.clone())?;
    let local = registry.resolve("local/m")?;
    let (request, dispatch) = request(&local)?;
    local
        .generate(request, dispatch)
        .try_collect::<Vec<_>>()
        .await?;
    assert!(
        !script.sent.lock().expect("fixture")[0]
            .headers
            .contains_key("authorization")
    );
    spec.authentication = Authentication::ApiKey;
    registry.register("ollama".into(), spec)?;
    assert!(
        registry.resolve("ollama/m").is_err(),
        "built-in names do not bypass catalog policy"
    );
    Ok(())
}

#[test]
fn serialized_catalogs_own_capacities_without_name_based_fallbacks() -> Result<()> {
    let (_, script) = provider(Adapter::Chat, 200);
    let catalog = default_catalog();
    let encoded = serde_json::to_vec(&catalog).expect("catalog");
    let catalog: BTreeMap<String, ProviderSpec> =
        serde_json::from_slice(&encoded).expect("catalog round trip");
    let mut registry = Registry::with_transport(
        Credentials::explicit(BTreeMap::from([("openai".into(), "fixture".into())])),
        script,
    );
    for (id, spec) in catalog {
        registry.register(id, spec)?;
    }
    let mut openai = registry.catalog()["openai"].as_ref().clone();
    openai.models.clear();
    registry.register("openai".into(), openai.clone())?;
    assert!(registry.resolve("openai/gpt-5").is_err());
    openai.models.insert(
        "gpt-5".into(),
        ModelContextCapacity {
            context_tokens: 32768,
            output_tokens: 2048,
        },
    );
    registry.register("openai".into(), openai)?;
    let model = registry.resolve("openai/gpt-5")?;
    assert_eq!(model.context_capacity(model.model())?.context_tokens, 32768);
    Ok(())
}

#[test]
fn resolution_binds_options_and_rejects_mutated_model_identity() -> Result<()> {
    let (_, script) = provider(Adapter::Responses, 200);
    let registry = Registry::with_transport(
        Credentials::explicit(BTreeMap::from([
            ("openai".into(), "fixture".into()),
            ("anthropic".into(), "fixture".into()),
        ])),
        script,
    );
    for (selection, options, field) in [
        (
            "openai/gpt-5",
            json!({"reasoning":{"effort":"high"},"text":{"format":{"type":"json_object"}}}),
            "reasoning",
        ),
        (
            "anthropic/claude-sonnet-4-5",
            json!({"thinking":{"type":"enabled","budget_tokens":2048}}),
            "thinking",
        ),
    ] {
        let provider = registry.resolve_with_options(selection, options.clone())?;
        let (prepared, _) = request(&provider)?;
        assert_eq!(provider.model().options, options);
        assert_eq!(
            projection::body(&provider, prepared.request())?[field],
            options[field]
        );
        let mut changed = provider.model().clone();
        changed.options = json!({});
        assert!(provider.context_capacity(&changed).is_err());
        assert!(provider.count_tokens(&prepared).is_ok());
    }
    assert!(
        registry
            .resolve_with_options("openai/gpt-5", json!([]))
            .is_err()
    );
    Ok(())
}

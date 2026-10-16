use crate::{Adapter, Authentication, ImageInput, ProviderSpec};
use acyclic_harness::context::ModelContextCapacity;
use std::collections::BTreeMap;

const ENDPOINTS: &[(&str, Adapter, Authentication, &str)] = &[
    (
        "anthropic",
        Adapter::Anthropic,
        Authentication::ApiKey,
        "https://api.anthropic.com/v1",
    ),
    (
        "openai",
        Adapter::Responses,
        Authentication::ApiKey,
        "https://api.openai.com/v1",
    ),
    (
        "openrouter",
        Adapter::Chat,
        Authentication::ApiKey,
        "https://openrouter.ai/api/v1",
    ),
    (
        "ollama",
        Adapter::Chat,
        Authentication::Optional,
        "http://localhost:11434/v1",
    ),
    (
        "lmstudio",
        Adapter::Chat,
        Authentication::Optional,
        "http://localhost:1234/v1",
    ),
    (
        "vllm",
        Adapter::Chat,
        Authentication::Optional,
        "http://localhost:8000/v1",
    ),
    (
        "gemini",
        Adapter::Gemini,
        Authentication::ApiKey,
        "https://generativelanguage.googleapis.com/v1beta",
    ),
];

const MODELS: &[(&str, &str, u32, u32)] = &[
    ("anthropic", "claude-sonnet-4-5", 200_000, 64_000),
    ("anthropic", "claude-sonnet-4-5-20250929", 200_000, 64_000),
    ("gemini", "gemini-2.5-pro", 1_048_576, 65_536),
    ("gemini", "gemini-2.5-flash", 1_048_576, 65_536),
    ("openai", "gpt-4.1", 1_047_576, 32_768),
    ("openai", "gpt-4.1-2025-04-14", 1_047_576, 32_768),
    ("openai", "gpt-5", 400_000, 128_000),
    ("openai", "gpt-5-2025-08-07", 400_000, 128_000),
];

/// Standard endpoint policies and exact model capacities as ordinary data.
pub fn default_catalog() -> BTreeMap<String, ProviderSpec> {
    ENDPOINTS
        .iter()
        .map(|&(id, adapter, authentication, base_url)| {
            let models: BTreeMap<String, ModelContextCapacity> = MODELS
                .iter()
                .filter(|(provider, ..)| *provider == id)
                .map(|&(_, name, context_tokens, output_tokens)| {
                    (
                        name.into(),
                        ModelContextCapacity {
                            context_tokens,
                            output_tokens,
                        },
                    )
                })
                .collect();
            (
                id.into(),
                ProviderSpec {
                    adapter,
                    authentication,
                    base_url: base_url.into(),
                    images: models
                        .keys()
                        .map(|name| {
                            (
                                name.clone(),
                                ImageInput {
                                    maximum_bytes: 4 * 1024 * 1024,
                                    maximum_total_bytes: 8 * 1024 * 1024,
                                    maximum_images: 16,
                                    maximum_dimension: 4096,
                                    maximum_tokens: 16_384,
                                },
                            )
                        })
                        .collect(),
                    models,
                    max_event_bytes: 1024 * 1024,
                },
            )
        })
        .collect()
}

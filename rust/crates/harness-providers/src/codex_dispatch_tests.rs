use crate::*;
use acyclic_harness::model::ModelProvider as _;
use acyclic_stream::BoxProviderFuture;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures::TryStreamExt as _;
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use time::OffsetDateTime;

const RESPONSE: &str = concat!(
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"run\",\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\n"
);

struct Server {
    replies: Mutex<VecDeque<(u16, String)>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl HttpTransport for Server {
    fn send(
        &self,
        request: HttpRequest,
    ) -> BoxProviderFuture<'_, acyclic_harness::Result<HttpResponse>> {
        Box::pin(async move {
            self.requests
                .lock()
                .map_err(|_| invalid("fixture poisoned"))?
                .push(request);
            let (status, body) = self
                .replies
                .lock()
                .map_err(|_| invalid("fixture poisoned"))?
                .pop_front()
                .ok_or_else(|| invalid("unexpected HTTP request"))?;
            Ok(HttpResponse {
                status,
                body: Box::pin(futures::stream::once(async { Ok(Bytes::from(body)) })),
            })
        })
    }
}

fn fixture(
    replies: Vec<(u16, String)>,
) -> acyclic_harness::Result<(tempfile::TempDir, CodexLogin, Arc<Server>, String)> {
    let root = tempfile::tempdir().map_err(|error| invalid(error.to_string()))?;
    let token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(
            json!({
                "exp":OffsetDateTime::now_utc().unix_timestamp() + 3600
            })
            .to_string()
        )
    );
    let path = root.path().join("auth.json");
    std::fs::write(
        &path,
        json!({"auth_mode":"chatgpt","tokens":{
            "access_token":token,"refresh_token":"refresh","account_id":"account"
        }})
        .to_string(),
    )
    .map_err(|error| invalid(error.to_string()))?;
    Ok((
        root,
        CodexLogin::from_path(path),
        Arc::new(Server {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        }),
        token,
    ))
}

#[tokio::test]
async fn cli_unauthorized_recovery_refreshes_then_streams_the_same_prepared_request()
-> acyclic_harness::Result<()> {
    let (_root, login, server, initial) = fixture(vec![
        (401, String::new()),
        (200, json!({"access_token":"new-access"}).to_string()),
        (200, RESPONSE.into()),
    ])?;
    let provider = Registry::with_transport(Credentials::explicit(BTreeMap::new()), server.clone())
        .with_codex_login(login)
        .resolve("openai/gpt-5")?;
    let (request, dispatch) = crate::tests::request(&provider)?;
    let events = provider
        .generate(request, dispatch)
        .try_collect::<Vec<_>>()
        .await?;
    assert_eq!(events.len(), 2);
    let requests = server
        .requests
        .lock()
        .map_err(|_| invalid("fixture poisoned"))?;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].url,
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        requests[0].headers["authorization"],
        format!("Bearer {initial}")
    );
    assert_eq!(requests[0].headers["chatgpt-account-id"], "account");
    assert_eq!(requests[1].url, "https://auth.openai.com/oauth/token");
    assert_eq!(requests[2].headers["authorization"], "Bearer new-access");
    assert_eq!(requests[0].body, requests[2].body);
    let body = crate::json(&requests[2].body)?;
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert!(body.get("max_output_tokens").is_none());
    Ok(())
}

#[tokio::test]
async fn api_key_precedence_uses_public_responses_without_reading_cli_login()
-> acyclic_harness::Result<()> {
    let (root, _login, server, _) = fixture(vec![(200, RESPONSE.into())])?;
    let login = CodexLogin::from_path(root.path().join("absent-auth.json"));
    let provider = Registry::with_transport(
        Credentials::explicit(BTreeMap::from([("openai".into(), "api-key".into())])),
        server.clone(),
    )
    .with_codex_login(login)
    .resolve("openai/gpt-5")?;
    let (request, dispatch) = crate::tests::request(&provider)?;
    provider
        .generate(request, dispatch)
        .try_collect::<Vec<_>>()
        .await?;
    let requests = server
        .requests
        .lock()
        .map_err(|_| invalid("fixture poisoned"))?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://api.openai.com/v1/responses");
    assert_eq!(requests[0].headers["authorization"], "Bearer api-key");
    assert!(!requests[0].headers.contains_key("chatgpt-account-id"));
    assert_eq!(crate::json(&requests[0].body)?["max_output_tokens"], 64);
    Ok(())
}

#[tokio::test]
async fn a_second_unauthorized_response_is_terminal() -> acyclic_harness::Result<()> {
    let (_root, login, server, _) = fixture(vec![
        (401, String::new()),
        (200, json!({"access_token":"new-access"}).to_string()),
        (401, String::new()),
    ])?;
    let provider = Registry::with_transport(Credentials::explicit(BTreeMap::new()), server.clone())
        .with_codex_login(login)
        .resolve("openai/gpt-5")?;
    let (request, dispatch) = crate::tests::request(&provider)?;
    assert!(
        provider
            .generate(request, dispatch)
            .try_collect::<Vec<_>>()
            .await
            .is_err()
    );
    assert_eq!(
        server
            .requests
            .lock()
            .map_err(|_| invalid("fixture poisoned"))?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn cli_credentials_cannot_be_forwarded_to_custom_endpoints() -> acyclic_harness::Result<()> {
    let (_root, login, server, _) = fixture(Vec::new())?;
    let mut registry = Registry::with_transport(Credentials::explicit(BTreeMap::new()), server)
        .with_codex_login(login);
    registry.register(
        "openai".into(),
        ProviderConfig {
            adapter: Adapter::Responses,
            base_url: "https://other.test/v1".into(),
            models: BTreeMap::new(),
            max_event_bytes: 4096,
        },
    )?;
    assert!(registry.resolve("openai/gpt-5").is_err());
    Ok(())
}

#[tokio::test]
async fn resolving_again_accepts_a_new_cli_account_without_switching_an_active_model()
-> acyclic_harness::Result<()> {
    let (root, login, server, _) = fixture(vec![(200, RESPONSE.into()), (200, RESPONSE.into())])?;
    let registry = Registry::with_transport(Credentials::explicit(BTreeMap::new()), server.clone())
        .with_codex_login(login);
    let active = registry.resolve("openai/gpt-5")?;
    let (request, dispatch) = crate::tests::request(&active)?;
    active
        .generate(request, dispatch)
        .try_collect::<Vec<_>>()
        .await?;
    let path = root.path().join("auth.json");
    let mut auth = crate::json(&std::fs::read(&path).map_err(|e| invalid(e.to_string()))?)?;
    auth["tokens"]["account_id"] = json!("new-account");
    std::fs::write(path, auth.to_string()).map_err(|e| invalid(e.to_string()))?;
    let (request, dispatch) = crate::tests::request(&active)?;
    assert!(
        active
            .generate(request, dispatch)
            .try_collect::<Vec<_>>()
            .await
            .is_err()
    );
    let selected = registry.resolve("openai/gpt-5")?;
    let (request, dispatch) = crate::tests::request(&selected)?;
    selected
        .generate(request, dispatch)
        .try_collect::<Vec<_>>()
        .await?;
    let requests = server
        .requests
        .lock()
        .map_err(|_| invalid("fixture poisoned"))?;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].headers["chatgpt-account-id"], "account");
    assert_eq!(requests[1].headers["chatgpt-account-id"], "new-account");
    Ok(())
}

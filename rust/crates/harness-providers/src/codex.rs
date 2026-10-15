use crate::{HttpRequest, HttpTransport, invalid};
use acyclic_harness::Result;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::StreamExt as _;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const MAX_AUTH_BYTES: u64 = 1024 * 1024;

/// Native-only reuse of an existing Codex CLI ChatGPT login.
/// Refreshes serialize in memory and defer to concurrent CLI writes.
pub struct CodexLogin {
    path: PathBuf,
    account: OnceLock<Option<String>>,
    refresh: crate::sync::AsyncMutex<()>,
}

impl CodexLogin {
    /// Reuses this auth.json; never starts an interactive login flow.
    pub fn from_path(path: PathBuf) -> Self {
        Self {
            path,
            account: OnceLock::new(),
            refresh: crate::sync::AsyncMutex::new(()),
        }
    }

    pub(crate) fn fresh(&self) -> Self {
        Self::from_path(self.path.clone())
    }

    /// Finds `$CODEX_HOME/auth.json` or the current user's `~/.codex/auth.json`.
    pub fn discover() -> Option<Self> {
        let root = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .map(|home| PathBuf::from(home).join(".codex"))
            })?;
        let path = root.join("auth.json");
        path.is_file().then(|| Self::from_path(path))
    }

    /// The rejected bearer is supplied only following HTTP 401, before any SSE is read.
    pub(crate) async fn bearer(
        &self,
        transport: &dyn HttpTransport,
        rejected: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        crate::refresh::serialized(&self.refresh, self.refresh_bearer(transport, rejected)).await
    }

    async fn refresh_bearer(
        &self,
        transport: &dyn HttpTransport,
        rejected: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        let (mut original, mut auth) = self.load().await?;
        self.check_account(&auth)?;
        for attempt in 0..2 {
            let access = token(&auth, "access_token")?;
            let force = rejected.is_some_and(|rejected| rejected == access);
            if !force && !needs_refresh(&auth, OffsetDateTime::now_utc()) {
                break;
            }
            let refresh = token(&auth, "refresh_token")?;
            let response = transport.send(HttpRequest {
                url: "https://auth.openai.com/oauth/token".into(),
                headers: BTreeMap::new(),
                body: serde_json::to_vec(&json!({"grant_type":"refresh_token", "client_id":CLIENT_ID,
                    "refresh_token":refresh,"refresh_reason":if force {"unauthorized"} else {"proactive_expiry"}}))
                    .map_err(|_| invalid("Codex refresh encoding failed"))?.into(),
            }).await?;
            if response.status != 200 {
                if attempt == 0 && matches!(response.status, 400 | 401) {
                    let (bytes, current) = self.load().await?;
                    self.check_account(&current)?;
                    if token(&current, "refresh_token").ok() != Some(refresh)
                        || token(&current, "access_token").ok() != Some(access)
                    {
                        original = bytes;
                        auth = current;
                        continue;
                    }
                }
                return Err(if matches!(response.status, 400 | 401 | 403) {
                    acyclic_harness::Error::Unauthorized(format!(
                        "Codex refresh HTTP status {}; sign in with the Codex CLI again",
                        response.status
                    ))
                } else {
                    crate::http_error(response.status)
                });
            }
            let mut body = response.body;
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                if chunk.len() as u64 > MAX_AUTH_BYTES.saturating_sub(bytes.len() as u64) {
                    return Err(invalid("Codex refresh response exceeds limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            let updated = crate::json(&bytes)?;
            if !["access_token", "refresh_token", "id_token"]
                .iter()
                .any(|field| updated.get(field).is_some())
            {
                return Err(invalid("Codex refresh did not return any tokens"));
            }
            for field in ["access_token", "refresh_token", "id_token"] {
                if let Some(value) = updated.get(field) {
                    if value.as_str().is_none_or(|token| token.trim().is_empty()) {
                        return Err(invalid("Codex refresh returned an invalid token"));
                    }
                    auth["tokens"][field] = value.clone();
                }
            }
            auth["last_refresh"] = json!(
                OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .map_err(|_| invalid("Codex refresh timestamp failed"))?
            );
            let path = self.path.clone();
            // Return the same document after atomic persistence, avoiding a clone of secrets.
            auth =
                tokio::task::spawn_blocking(move || persist_if_unchanged(&path, &original, auth))
                    .await
                    .map_err(|_| invalid("Codex credential writer interrupted"))??;
            self.check_account(&auth)?;
            break;
        }
        let access = token(&auth, "access_token")?.to_owned();
        Ok((access, self.account.get().cloned().flatten()))
    }

    async fn load(&self) -> Result<(Vec<u8>, Value)> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || load(&path))
            .await
            .map_err(|_| invalid("Codex credential reader interrupted"))?
    }

    fn check_account(&self, auth: &Value) -> Result<()> {
        let current = auth["tokens"]["account_id"].as_str().map(str::to_owned);
        if self.account.get_or_init(|| current.clone()) != &current {
            return Err(acyclic_harness::Error::Unauthorized(
                "Codex login account changed; resolve the model again".into(),
            ));
        }
        Ok(())
    }
}

fn token<'a>(auth: &'a Value, field: &str) -> Result<&'a str> {
    auth["tokens"][field]
        .as_str()
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| invalid("Codex CLI ChatGPT login is incomplete; sign in again"))
}

fn needs_refresh(auth: &Value, now: OffsetDateTime) -> bool {
    let expiry = auth["tokens"]["access_token"]
        .as_str()
        .and_then(|token| token.split('.').nth(1))
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|payload| serde_json::from_slice::<Value>(&payload).ok())
        .and_then(|claims| claims["exp"].as_i64());
    if let Some(expiry) = expiry {
        return expiry <= now.unix_timestamp().saturating_add(300);
    }
    auth["last_refresh"]
        .as_str()
        .and_then(|date| OffsetDateTime::parse(date, &Rfc3339).ok())
        .is_none_or(|last| now - last >= time::Duration::days(8))
}

fn load(path: &Path) -> Result<(Vec<u8>, Value)> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| invalid("cannot read Codex auth.json"))?
        .take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("cannot read Codex auth.json"))?;
    let auth = decode_auth(&bytes)?;
    Ok((bytes, auth))
}

pub(crate) fn decode_auth(bytes: &[u8]) -> Result<Value> {
    if bytes.len() as u64 > MAX_AUTH_BYTES {
        return Err(invalid("Codex auth.json exceeds limit"));
    }
    let auth = crate::json(bytes)?;
    if auth["auth_mode"]
        .as_str()
        .is_some_and(|mode| mode != "chatgpt")
    {
        return Err(invalid("Codex auth.json is not a ChatGPT CLI login"));
    }
    token(&auth, "access_token")?;
    Ok(auth)
}

fn persist_if_unchanged(path: &Path, original: &[u8], auth: Value) -> Result<Value> {
    persist_with(path, original, auth, || Ok(()))
}

fn persist_with(
    path: &Path,
    original: &[u8],
    auth: Value,
    before_replace: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    let absolute = if path.is_absolute() {
        None
    } else {
        Some(std::path::absolute(path).map_err(|_| invalid("cannot resolve Codex auth path"))?)
    };
    let path = absolute.as_deref().unwrap_or(path);
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Codex auth path needs a parent"))?;
    // Stage beside CODEX_HOME, leaving its directory containing only auth.json.
    let staging = parent
        .parent()
        .ok_or_else(|| invalid("Codex auth directory needs a parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(staging)
        .map_err(|_| invalid("cannot create Codex credential update"))?;
    serde_json::to_writer(file.as_file_mut(), &auth)
        .map_err(|_| invalid("cannot encode Codex credential update"))?;
    file.write_all(b"\n")
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| invalid("cannot flush Codex credential update"))?;
    let (bytes, current) = load(path)?;
    if bytes != original {
        return Ok(current);
    }
    before_replace()?;
    // The CLI does not cooperate with a lock. Compare the whole document (also
    // preserving non-token CLI edits), after staging/fsync and immediately before
    // rename. This is best-effort: a CLI write during this final read + rename
    // window cannot be excluded by a portable filesystem conditional replacement.
    let (bytes, current) = load(path)?;
    if bytes != original {
        return Ok(current);
    }
    file.persist(path)
        .map_err(|_| invalid("cannot replace Codex auth.json"))?;
    Ok(auth)
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;

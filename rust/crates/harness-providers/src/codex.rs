use crate::{HttpRequest, HttpTransport, invalid};
use acyclic_harness::Result;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt as _;
use futures::StreamExt as _;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const MAX_AUTH_BYTES: u64 = 1024 * 1024;

/// Native-only reuse of an existing Codex CLI ChatGPT login.
/// Refreshes are serialized with both an async mutex and a process file lock.
pub struct CodexLogin {
    path: PathBuf,
    account: tokio::sync::Mutex<Option<Option<String>>>,
}

impl CodexLogin {
    /// Reuses this auth.json; never starts an interactive login flow.
    pub fn from_path(path: PathBuf) -> Self {
        Self {
            path,
            account: tokio::sync::Mutex::new(None),
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
        let mut account = self.account.lock().await;
        let path = self.path.clone();
        // Lock acquisition can block on another process performing a refresh.
        let (lock, mut auth) = tokio::task::spawn_blocking(move || load_locked(&path))
            .await
            .map_err(|_| invalid("Codex credential reader interrupted"))??;
        let current_account = auth["tokens"]["account_id"].as_str().map(str::to_owned);
        if account
            .as_ref()
            .is_some_and(|expected| expected != &current_account)
        {
            return Err(invalid(
                "Codex login account changed; resolve the model again",
            ));
        }
        *account = Some(current_account.clone());
        let access = token(&auth, "access_token")?;
        let force = rejected.is_some_and(|rejected| rejected == access);
        if force || needs_refresh(&auth, OffsetDateTime::now_utc()) {
            let refresh = token(&auth, "refresh_token")?;
            let response = transport.send(HttpRequest {
                url: "https://auth.openai.com/oauth/token".into(),
                headers: BTreeMap::new(),
                body: serde_json::to_vec(&json!({"grant_type":"refresh_token", "client_id":CLIENT_ID,
                    "refresh_token":refresh,"refresh_reason":if force {"unauthorized"} else {"proactive_expiry"}}))
                    .map_err(|_| invalid("Codex refresh encoding failed"))?,
            }).await?;
            if response.status != 200 {
                return Err(invalid(if matches!(response.status, 400 | 401 | 403) {
                    "Codex login refresh rejected; sign in with the Codex CLI again"
                } else {
                    "Codex login refresh temporarily unavailable"
                }));
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
            auth = tokio::task::spawn_blocking(move || {
                persist(&path, &auth)?;
                Ok::<_, acyclic_harness::Error>(auth)
            })
            .await
            .map_err(|_| invalid("Codex credential writer interrupted"))??;
        }
        let access = token(&auth, "access_token")?.to_owned();
        drop(lock);
        Ok((access, current_account))
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

fn load_locked(path: &Path) -> Result<(File, Value)> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Codex auth path needs a parent"))?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let lock = options
        .open(parent.join("acyclic-auth.lock"))
        .map_err(|_| invalid("cannot open Codex credential lock"))?;
    lock.lock_exclusive()
        .map_err(|_| invalid("cannot lock Codex credentials"))?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| invalid("cannot read Codex auth.json"))?
        .take(MAX_AUTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("cannot read Codex auth.json"))?;
    if bytes.len() as u64 > MAX_AUTH_BYTES {
        return Err(invalid("Codex auth.json exceeds limit"));
    }
    let auth = crate::json(&bytes)?;
    if auth["auth_mode"]
        .as_str()
        .is_some_and(|mode| mode != "chatgpt")
    {
        return Err(invalid("Codex auth.json is not a ChatGPT CLI login"));
    }
    token(&auth, "access_token")?;
    Ok((lock, auth))
}

fn persist(path: &Path, auth: &Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Codex auth path needs a parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| invalid("cannot create Codex credential update"))?;
    serde_json::to_writer(file.as_file_mut(), auth)
        .map_err(|_| invalid("cannot encode Codex credential update"))?;
    file.write_all(b"\n")
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| invalid("cannot flush Codex credential update"))?;
    file.persist(path)
        .map_err(|_| invalid("cannot replace Codex auth.json"))?;
    Ok(())
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;

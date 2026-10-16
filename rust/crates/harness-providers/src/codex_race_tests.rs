use super::*;
use std::collections::VecDeque;

#[test]
fn cli_write_after_comparison_before_persist_is_adopted() -> Result<()> {
    for account_switch in [false, true] {
        let (root, login, _) = fixture(&jwt(0), 200)?;
        let path = root.path().join("auth.json");
        let (original, _) = load(&path, &mut CredentialIo::new(None)?)?;
        let mut cli = auth("cli-access");
        cli["tokens"]["refresh_token"] = json!("cli-rotated");
        if account_switch {
            cli["tokens"]["account_id"] = json!("other-account");
        }
        let cli_bytes = serde_json::to_vec(&cli).expect("fixture encoding");
        login.check_account(&auth("original"))?;
        let adopted = persist_with(
            &path,
            &original,
            auth("provider-access"),
            || {
                // Exact former race: the first comparison has already succeeded.
                let staged: Vec<_> = std::fs::read_dir(root.path())
                    .expect("fixture directory")
                    .map(|entry| entry.expect("fixture entry").path())
                    .filter(|entry| entry != &path)
                    .collect();
                assert_eq!(staged.len(), 1, "stage inside the auth directory");
                assert_eq!(staged[0].parent(), path.parent());
                std::fs::write(&path, &cli_bytes).map_err(|_| invalid("fixture write"))
            },
            &mut CredentialIo::new(None)?,
        )?;
        assert_eq!(adopted, cli);
        assert_eq!(std::fs::read(&path).expect("fixture read"), cli_bytes);
        assert_eq!(login.check_account(&adopted).is_err(), account_switch);
        assert_eq!(
            std::fs::read_dir(root.path())
                .expect("fixture directory")
                .count(),
            1
        );
    }
    Ok(())
}

struct RacingCli {
    path: PathBuf,
    cli_bytes: Vec<u8>,
    replies: Mutex<VecDeque<(u16, Value)>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl HttpTransport for RacingCli {
    fn send(&self, request: HttpRequest) -> BoxProviderFuture<'_, Result<HttpResponse>> {
        Box::pin(async move {
            let first = {
                let mut requests = self.requests.lock().map_err(|_| invalid("fixture lock"))?;
                requests.push(request);
                requests.len() == 1
            };
            if first {
                // The real CLI does not participate in this provider's in-memory mutex.
                // Publish a complete credential document while independent readers run.
                let mut update = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())
                    .map_err(|_| invalid("fixture staging"))?;
                update
                    .write_all(&self.cli_bytes)
                    .map_err(|_| invalid("fixture write"))?;
                update
                    .persist(&self.path)
                    .map_err(|_| invalid("fixture replace"))?;
            }
            let (status, value) = self
                .replies
                .lock()
                .map_err(|_| invalid("fixture lock"))?
                .pop_front()
                .ok_or_else(|| invalid("unexpected refresh"))?;
            let bytes = Bytes::from(value.to_string());
            Ok(HttpResponse {
                status,
                body: Box::pin(futures::stream::once(async { Ok(bytes) })),
            })
        })
    }
}

fn race(path: &Path, cli: Value, replies: Vec<(u16, Value)>) -> RacingCli {
    RacingCli {
        path: path.to_owned(),
        cli_bytes: format!("{cli}\n").into_bytes(),
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
    }
}

#[tokio::test]
async fn cli_write_during_successful_refresh_wins_without_extra_auth_directory_files() -> Result<()>
{
    let (root, login, _) = fixture(&jwt(0), 200)?;
    let cli_access = jwt(OffsetDateTime::now_utc().unix_timestamp() + 7200);
    let mut cli = auth(&cli_access);
    cli["tokens"]["refresh_token"] = json!("cli-refresh");
    cli["unrelated_cli_field"] = json!([1, 2, 3]);
    let transport = race(
        &root.path().join("auth.json"),
        cli,
        vec![(
            200,
            json!({"access_token":"discard-provider-access", "refresh_token":"discard-provider-refresh"}),
        )],
    );
    assert_eq!(login.bearer(&transport, None, None).await?.0, cli_access);
    assert_eq!(
        std::fs::read(&transport.path).map_err(|_| invalid("fixture read"))?,
        transport.cli_bytes
    );
    assert_eq!(
        std::fs::read_dir(root.path())
            .map_err(|_| invalid("fixture directory"))?
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn rejected_refresh_reloads_a_fresh_cli_bearer_once() -> Result<()> {
    for status in [400, 401] {
        let (root, login, _) = fixture(&jwt(0), 200)?;
        let cli_access = jwt(OffsetDateTime::now_utc().unix_timestamp() + 7200);
        let transport = race(
            &root.path().join("auth.json"),
            auth(&cli_access),
            vec![(status, json!({"error":"invalid_grant"}))],
        );
        assert_eq!(login.bearer(&transport, None, None).await?.0, cli_access);
        assert_eq!(
            transport
                .requests
                .lock()
                .map_err(|_| invalid("fixture lock"))?
                .len(),
            1
        );
        assert_eq!(
            std::fs::read(&transport.path).map_err(|_| invalid("fixture read"))?,
            transport.cli_bytes
        );
    }
    Ok(())
}

#[tokio::test]
async fn rejected_refresh_retries_the_changed_on_disk_refresh_token() -> Result<()> {
    for status in [400, 401] {
        let (root, login, _) = fixture(&jwt(0), 200)?;
        let mut cli = auth(&jwt(0));
        cli["tokens"]["refresh_token"] = json!("rotated-by-cli");
        let transport = race(
            &root.path().join("auth.json"),
            cli,
            vec![
                (status, json!({"error":"invalid_grant"})),
                (
                    200,
                    json!({"access_token":"updated", "refresh_token":"rotated-again"}),
                ),
            ],
        );
        assert_eq!(login.bearer(&transport, None, None).await?.0, "updated");
        let requests = transport
            .requests
            .lock()
            .map_err(|_| invalid("fixture lock"))?;
        assert_eq!(requests.len(), 2);
        assert_eq!(
            crate::json(&requests[1].body)?["refresh_token"],
            "rotated-by-cli"
        );
    }
    Ok(())
}

#[tokio::test]
async fn cli_account_changes_during_refresh_are_rejected_without_overwriting_the_cli() -> Result<()>
{
    for status in [200, 400, 401] {
        let (root, login, _) = fixture(&jwt(0), 200)?;
        let mut cli = auth(&jwt(OffsetDateTime::now_utc().unix_timestamp() + 7200));
        cli["tokens"]["account_id"] = json!("another-account");
        let transport = race(
            &root.path().join("auth.json"),
            cli,
            vec![(status, json!({"access_token":"provider-token"}))],
        );
        assert!(matches!(
            login.bearer(&transport, None, None).await,
            Err(acyclic_harness::Error::Unauthorized(_))
        ));
        assert_eq!(
            std::fs::read(&transport.path).map_err(|_| invalid("fixture read"))?,
            transport.cli_bytes
        );
    }
    Ok(())
}

#[tokio::test]
async fn independent_login_instances_compare_and_swap_against_the_same_cli_file() -> Result<()> {
    let (root, first, _) = fixture(&jwt(0), 200)?;
    let second = CodexLogin::from_path(root.path().join("auth.json"));
    let cli_access = jwt(OffsetDateTime::now_utc().unix_timestamp() + 7200);
    let transport = race(
        &root.path().join("auth.json"),
        auth(&cli_access),
        vec![
            (200, json!({"access_token":"discard-first"})),
            (200, json!({"access_token":"discard-second"})),
        ],
    );
    let (left, right) = futures::join!(
        first.bearer(&transport, None, None),
        second.bearer(&transport, None, None)
    );
    assert_eq!(left?.0, cli_access);
    assert_eq!(right?.0, cli_access);
    assert_eq!(
        std::fs::read(&transport.path).map_err(|_| invalid("fixture read"))?,
        transport.cli_bytes
    );
    Ok(())
}

#[tokio::test]
async fn oauth_transient_statuses_keep_the_retryable_classification() -> Result<()> {
    for status in [429, 500, 503] {
        let (_root, login, transport) = fixture(&jwt(0), status)?;
        let error = login
            .bearer(&transport, None, None)
            .await
            .expect_err("transient failure");
        assert!(error.to_string().contains(&status.to_string()));
        assert!(matches!(error, acyclic_harness::Error::Storage(_)));
    }
    Ok(())
}

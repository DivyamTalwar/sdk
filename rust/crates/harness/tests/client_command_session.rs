use super::*;
use acyclic_harness::{
    AgentId, Capabilities, OperationId, TaskId,
    conversation::{ContentGrant, VolumeClass, VolumeOperation, VolumeOwner, VolumeRef},
    core::{AggregateKind, Authority, AuthorityIssuer},
    filesystem::FilesystemHost,
    resources::ProviderRef,
    client::{CommandFileRoot, CommandSession, CommandTool},
    task_volume::{Component, FileKey, FileValue, Query, TaskVolume},
};
use acyclic_stream::{MemoryStream, StreamPath};
use futures::FutureExt as _;
use proptest::prelude::*;
use std::sync::Arc;

fn session_id() -> uuid::Uuid {
    uuid::Uuid::from_bytes([7; 16])
}

async fn volume() -> Result<Arc<TaskVolume<MemoryStream>>> {
    Ok(Arc::new(
        TaskVolume::open(
            Arc::new(MemoryStream::default()),
            StreamPath::new("command-session").unwrap(),
        )
        .await
        .map_err(|error| Error::Storage(error.to_string()))?,
    ))
}

async fn publish(volume: &TaskVolume<MemoryStream>, path: &str, bytes: Vec<u8>) -> Result<()> {
    let cut = volume
        .snapshot(&Query::Root, 128)
        .await
        .map_err(|error| Error::Storage(error.to_string()))?;
    let key = FileKey::parse(path).map_err(|error| Error::Invalid(error.to_string()))?;
    volume
        .append(cut.generation, &FileValue::new(key, bytes))
        .await
        .map_err(|error| Error::Storage(error.to_string()))?;
    Ok(())
}

#[cfg(feature = "filesystem-local")]
#[tokio::test]
async fn published_shell_request_causes_a_real_tool_effect_and_does_not_replay() -> Result<()> {
    acyclic_native_runtime::initialize_native_io_services()
        .map_err(|error| Error::Storage(error.to_string()))?;
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    client
        .write_state(
            CommandFileRoot::Config,
            "settings.json",
            &json!({"workspace":directory.path()}),
        )
        .await?;
    // The real command client publishes an immutable request file. The consumer
    // executes precisely that file through the same bounded host binding used
    // by GraphCoder, without a model or the removed text dispatcher.
    let script = if cfg!(windows) {
        "Add-Content -NoNewline effect.txt 'hi'; Write-Output hi"
    } else {
        "printf hi >> effect.txt; printf hi"
    };
    client
        .submit(&format!("/SH {script}"), &DeniedReader)
        .await?;
    let request = client.next_request().await?;
    let settings = client
        .state(CommandFileRoot::Config, "settings.json")?
        .unwrap();
    let run = |_, _, resolution| async move {
        let CommandResolution::Tool { tool, input } = resolution else {
            return Err(Error::Invalid("expected direct tool".into()));
        };
        acyclic_harness::client::execute_host_command(tool, &input, &settings, 32_768).await
    };
    let result = client.consume(&request, run).await?.unwrap();
    assert_eq!(result["stdout"].as_str().unwrap().trim(), "hi");
    assert_eq!(
        std::fs::read(directory.path().join("effect.txt")).unwrap(),
        b"hi"
    );
    assert!(
        client
            .consume(&request, |_, _, _| async {
                panic!("claimed request was resent")
            })
            .await?
            .is_none()
    );
    drop(client);
    let mut reopened = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    let recovered = reopened.next_request().await?;
    assert!(
        reopened
            .consume(&recovered, |_, _, _| async {
                panic!("reopening resent shell effect")
            })
            .await?
            .is_none()
    );
    assert_eq!(
        std::fs::read(directory.path().join("effect.txt")).unwrap(),
        b"hi"
    );
    let snapshot = volume.snapshot(&Query::Root, 128).await.unwrap();
    let results: Vec<_> = snapshot
        .files
        .iter()
        .filter(|file| file.key.path().ends_with("/result.json"))
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&results[0].bytes).unwrap()["ok"],
        result
    );
    Ok(())
}

#[cfg(all(feature = "filesystem-local", unix))]
#[tokio::test]
async fn graphcoder_shell_runs_through_file_dispatch_without_a_model()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::{io::Write as _, process::Stdio, time::Duration};
    acyclic_native_runtime::initialize_native_io_services()?;
    let directory = tempfile::tempdir()?;
    let data: Value = serde_json::from_str(include_str!(
        "../src/bin/graphcoder/native_scenario/data.json"
    ))?;
    let fs_provider = ProviderRef::new(
        "command-client".into(),
        "filesystem".into(),
        "2".into(),
        &mut None,
    )?;
    let stream_provider = ProviderRef::new(
        "command-client".into(),
        "stream".into(),
        "2".into(),
        &mut None,
    )?;
    let agent = AgentId::new();
    let volume = VolumeRef::new(
        fs_provider.clone(),
        "private".into(),
        VolumeClass::AgentPrivate,
        VolumeOwner::Agent(agent),
        &mut None,
    )?;
    let authority = Authority {
        kind: AggregateKind::Conversation,
        id: "command-client".into(),
    };
    let issuer = AuthorityIssuer::new("command-client", [9; 32], authority.clone());
    let mut grants: Vec<String> = serde_json::from_value(data["grants"].clone())?;
    grants.extend([
        volume.capability(VolumeOperation::Read, &mut None)?,
        volume.capability(VolumeOperation::Write, &mut None)?,
        volume.directory_read_capability("", &mut None)?,
    ]);
    let scope = issuer.issue(
        "terminal user",
        &Capabilities::new(grants),
        Some(agent),
        None,
        &mut None,
    )?;
    let fs_root = directory.path().join("filesystem");
    let host = FilesystemHost::new(
        acyclic_fs::Fs::local(acyclic_fs::LocalOptions::new(&fs_root)).await?,
        &fs_provider,
        None,
    )?;
    host.create_volume(&volume).await?;
    let writer = ContentGrant::verify(
        &issuer.verifier(&mut None)?,
        &scope,
        &volume,
        VolumeOperation::Write,
        32_768,
        &mut None,
    )?;
    host.put_content(
        &volume,
        &writer,
        ".claude/commands/greet.md",
        b"Hello $ARGUMENTS",
        "text/markdown",
        "greet",
        32_768,
        &IdempotencyKey::new("custom-command")?,
        None,
        None,
    )
    .await?;
    drop(host);
    let mut config = data["config"].clone();
    let fields = json!({
        "filesystem_root":fs_root,"stream_root":directory.path().join("stream"),
        "filesystem_provider":fs_provider,"stream_provider":stream_provider,"private_volume":volume,
        "owner":{"authority":authority,"issuer":"command-client","key":vec![9_u8;32],"scope":scope},
        "generation":"commands-test","model_invocation_ledger":directory.path().join("model.jsonl"),
        "session_limits":{"active_tasks":6,"depth":3,"model_steps":7,"total_tasks":12},
        "tools":{"content_budget":data["publication"]["budget"]},"command_workspace":directory.path(),
        "command_roots":[{"scope":"user","root":{"volume":volume,"directory":""}}],
        "deterministic_delay_ms":0
    });
    for (name, value) in fields.as_object().unwrap() {
        config[name] = value.clone();
    }
    let config_path = directory.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config)?)?;
    let mut input = tempfile::NamedTempFile::new_in(directory.path())?;
    input.write_all(b"/SH printf hi > cli-effect.txt; printf hi\n/KEYS\n/back 0\n/q\n")?;
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_graphcoder"));
    command
        .args(["--session", config_path.to_str().unwrap(), "--headless"])
        .stdin(Stdio::from(std::fs::File::open(input.path())?))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let result = acyclic_native_runtime::ProcessTree::spawn(&mut command)?
        .wait_with_output(
            Duration::from_secs(5),
            65_536,
            None,
            || Ok(()),
            |_| Ok(acyclic_native_runtime::ProcessInput::default()),
            None,
            None,
        )
        .map_err(|failure| failure.error)?;
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let rendered = String::from_utf8(result.stdout)?;
    assert!(rendered.contains("command-json="), "{rendered}");
    assert!(
        rendered.contains("/shell")
            && rendered.contains("/shortcuts")
            && rendered.contains("/greet"),
        "catalog help: {rendered}"
    );
    assert_eq!(
        std::fs::read(directory.path().join("cli-effect.txt"))?,
        b"hi"
    );
    assert!(std::fs::read(directory.path().join("model.jsonl"))?.is_empty());
    std::fs::write(input.path(), b"/does-not-exist\n")?;
    let mut unknown = std::process::Command::new(env!("CARGO_BIN_EXE_graphcoder"));
    unknown
        .args(["--session", config_path.to_str().unwrap(), "--headless"])
        .stdin(Stdio::from(std::fs::File::open(input.path())?))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let rejected = acyclic_native_runtime::ProcessTree::spawn(&mut unknown)?
        .wait_with_output(
            Duration::from_secs(4),
            65_536,
            None,
            || Ok(()),
            |_| Ok(acyclic_native_runtime::ProcessInput::default()),
            None,
            None,
        )
        .map_err(|failure| failure.error)?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8(rejected.stderr)?.contains("Unknown command: /does-not-exist"));
    assert!(std::fs::read(directory.path().join("model.jsonl"))?.is_empty());
    Ok(())
}

#[tokio::test]
async fn catalog_and_external_requests_are_observed_through_the_cursor_follow() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    let mut definitions: Value =
        serde_json::from_slice(acyclic_harness::client::DEFAULT_COMMANDS_JSON).unwrap();
    let shell = definitions
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|spec| spec["name"] == "shell")
        .unwrap();
    shell["aliases"]
        .as_array_mut()
        .unwrap()
        .push(json!("external"));
    publish(
        &volume,
        "commands/catalog.json",
        serde_json::to_vec(&definitions).unwrap(),
    )
    .await?;
    client.observe_change().await?;
    assert_eq!(
        client.catalog().complete("EXTERNAL").next().unwrap().name,
        "shell"
    );
    publish(
        &volume,
        "commands/themes.json",
        br#"["Solarized","Light"]"#.to_vec(),
    )
    .await?;
    client.observe_change().await?;
    assert_eq!(
        client.catalog().complete_input("/THEME Sol")[0].replacement,
        "/theme Solarized"
    );
    client.submit("/THEME Solarized", &DeniedReader).await?;
    assert_eq!(
        client
            .state(CommandFileRoot::Config, "theme.json")?
            .unwrap()["name"],
        "Solarized"
    );
    let operation = OperationId::new();
    let resolution = CommandResolution::Tool {
        tool: CommandTool::Bash,
        input: json!({"command":"echo hi"}),
    };
    publish(
        &volume,
        &format!(".tasks/{}/requests/{operation}.json", session_id()),
        serde_json::to_vec(&resolution).unwrap(),
    )
    .await?;
    let request = client.next_request().await?;
    let result = client
        .consume(&request, |actual, task, intent| async move {
            assert_eq!(actual, operation);
            assert_eq!(task, Some(TaskId::from_bytes(session_id().into_bytes())));
            assert_eq!(intent, resolution);
            Ok(json!({"called":true}))
        })
        .await?;
    assert_eq!(result, Some(json!({"called":true})));
    Ok(())
}

#[cfg(feature = "tui")]
#[tokio::test]
async fn dispatch_and_input_follow_the_same_catalog_and_theme_files() -> Result<()> {
    use acyclic_harness::{tui::{Editor, PromptInput}, tui_files::CompletionFiles};
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    let mut input = PromptInput::new(Editor::new("/SH"));
    let mut completions = CompletionFiles::observe(&volume, &mut input, None, 128).await
        .map_err(|e| Error::Storage(e.to_string()))?;
    let candidate = input.completion.items().next().unwrap();
    assert_eq!(candidate.value, "sh");
    assert!(candidate.tool);
    assert!(input.completion.accept(&mut input.editor, 0));
    assert_eq!(input.editor.text(), "/sh ");
    publish(&volume, "commands/themes.json", br#"["Solarized Light"]"#.to_vec()).await?;
    client.observe_change().await?;
    completions.next(&mut input).await.map_err(|e| Error::Storage(e.to_string()))?;
    input.editor.set_text("/THEME sol", 1);
    input.refresh_completions();
    assert!(input.completion.accept(&mut input.editor, 2));
    assert_eq!(input.editor.text(), "/THEME Solarized Light");
    client.submit(input.editor.text(), &DeniedReader).await?;
    assert_eq!(client.state(CommandFileRoot::Config, "theme.json")?.unwrap()["name"], "Solarized Light");
    Ok(())
}

#[tokio::test]
async fn cancelling_a_claimed_dispatch_keeps_it_uncertain_without_resending() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    client.submit("/shell echo hi", &DeniedReader).await?;
    let request = client.next_request().await?;
    let dispatched = std::cell::Cell::new(false);
    assert!(
        client
            .consume(&request, |_, _, _| async {
                dispatched.set(true);
                std::future::pending::<Result<Value>>().await
            })
            .now_or_never()
            .is_none()
    );
    assert!(
        dispatched.get(),
        "negative test must reach the real claimed dispatch"
    );
    drop(client);
    let mut reopened = CommandSession::open(volume, session_id(), 32_768, 128).await?;
    let request = reopened.next_request().await?;
    assert!(matches!(
        reopened
            .consume(&request, |_, _, _| async {
                panic!("uncertain dispatch was resent")
            })
            .await,
        Err(Error::Indeterminate(_))
    ));
    Ok(())
}

#[tokio::test]
async fn a_stale_publisher_never_uses_old_catalog_or_settings() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    publish(
        &volume,
        ".ui/config/settings.json",
        b"{\"reasoning\":true}".to_vec(),
    )
    .await?;
    assert!(matches!(
        client.submit("/reasoning", &DeniedReader).await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        client
            .state(CommandFileRoot::Config, "settings.json")?
            .unwrap()["reasoning"],
        true
    );
    client.submit("/reasoning", &DeniedReader).await?;
    assert_eq!(
        client
            .state(CommandFileRoot::Config, "settings.json")?
            .unwrap()["reasoning"],
        false
    );
    Ok(())
}

#[tokio::test]
async fn archive_selection_and_recovery_fold_the_outcome_files() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    let mut definitions: Vec<Value> =
        serde_json::from_slice(acyclic_harness::client::DEFAULT_COMMANDS_JSON).unwrap();
    definitions.push(json!({"name":"back","description":"Archive","arguments":"text","action":{"kind":"sdk","command":"back"}}));
    client
        .install_catalog(serde_json::to_vec(&definitions).unwrap())
        .await?;
    for index in 0..2 {
        client.submit("/shell echo hi", &DeniedReader).await?;
        let request = client.next_request().await?;
        client
            .consume(&request, |_, _, _| async move {
                Ok(json!({"rendered":format!("page {index}")}))
            })
            .await?;
    }
    client.submit("/back 1", &DeniedReader).await?;
    assert_eq!(
        client
            .state(CommandFileRoot::Session, "layout.json")?
            .unwrap()["history_page"],
        1
    );
    assert_eq!(client.output_page(1)?.unwrap()["ok"]["rendered"], "page 0");
    assert!(client.output_page(usize::MAX)?.is_none());
    drop(client);
    let reopened = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    assert_eq!(
        reopened.output_page(0)?.unwrap()["ok"]["rendered"],
        "page 1"
    );
    let query = Query::folder(&format!(".sessions/{}", session_id())).unwrap();
    let executions = volume.snapshot(&query, 128).await.unwrap();
    assert_eq!(executions.files.len(), 6);
    assert!(executions.files.iter().all(|file| query.matches(&file.key)));
    Ok(())
}

#[tokio::test]
async fn invalid_catalog_publication_is_retained_until_explicit_snapshot_recovery() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    publish(&volume, "commands/catalog.json", b"not JSON".to_vec()).await?;
    publish(
        &volume,
        "commands/catalog.json",
        acyclic_harness::client::DEFAULT_COMMANDS_JSON.to_vec(),
    )
    .await?;
    assert!(client.observe_change().await.is_err());
    assert!(
        client.observe_change().await.is_err(),
        "a later publication cannot hide a rejected query cut"
    );
    drop(client);
    let mut reopened = CommandSession::open(volume, session_id(), 32_768, 128).await?;
    reopened.submit("/shell echo hi", &DeniedReader).await?;
    Ok(())
}

#[tokio::test]
async fn unrelated_malformed_ui_file_does_not_block_a_direct_request() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    publish(&volume, ".ui/config/theme.json", b"not JSON".to_vec()).await?;
    client.observe_change().await?;
    client.submit("/shell echo hi", &DeniedReader).await?;
    assert!(client.take_request().is_some());
    assert!(
        client.submit("/theme light", &DeniedReader).await.is_ok(),
        "unknown theme only opens the picker"
    );
    Ok(())
}

#[tokio::test]
async fn request_target_and_settings_survive_later_navigation() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume, session_id(), 32_768, 128).await?;
    let original = TaskId::from_bytes([3; 16]);
    let other = TaskId::from_bytes([4; 16]);
    client
        .write_state(
            CommandFileRoot::Session,
            "layout.json",
            &json!({"selected_task":original}),
        )
        .await?;
    client
        .write_state(
            CommandFileRoot::Task,
            "settings.json",
            &json!({"workspace":"original workspace"}),
        )
        .await?;
    client.submit("/shell echo hi", &DeniedReader).await?;
    let request = client.next_request().await?;
    client
        .write_state(
            CommandFileRoot::Session,
            "layout.json",
            &json!({"selected_task":other}),
        )
        .await?;
    client
        .write_state(
            CommandFileRoot::Task,
            "settings.json",
            &json!({"workspace":"other workspace"}),
        )
        .await?;
    assert_eq!(
        client.request_settings(&request)?["workspace"],
        "original workspace"
    );
    client
        .consume(&request, |_, target, _| async move {
            assert_eq!(target, Some(original));
            Ok(Value::Null)
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn session_and_task_requests_cannot_alias_even_with_equal_identifiers() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume, session_id(), 32_768, 128).await?;
    client.submit("/shell echo root", &DeniedReader).await?;
    let root = client.next_request().await?;
    assert!(matches!(root.key, FileKey::Session { .. }));
    client
        .consume(&root, |_, target, _| async move {
            assert_eq!(target, None);
            Ok(Value::Null)
        })
        .await?;
    let task = TaskId::from_bytes(session_id().into_bytes());
    client
        .write_state(
            CommandFileRoot::Session,
            "layout.json",
            &json!({"selected_task":task}),
        )
        .await?;
    client.submit("/shell echo task", &DeniedReader).await?;
    let selected = client.next_request().await?;
    assert!(matches!(selected.key, FileKey::Task { .. }));
    client
        .consume(&selected, |_, target, _| async move {
            assert_eq!(target, Some(task));
            Ok(Value::Null)
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn activity_selection_recovers_from_sdk_files_and_ignores_tool_output_fields() -> Result<()> {
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    let mut definitions: Vec<Value> =
        serde_json::from_slice(acyclic_harness::client::DEFAULT_COMMANDS_JSON).unwrap();
    definitions.push(json!({"name":"activity","description":"Activity","arguments":"text","action":{"kind":"sdk","command":"next"}}));
    client
        .install_catalog(serde_json::to_vec(&definitions).unwrap())
        .await?;
    client.submit("/activity", &DeniedReader).await?;
    let request = client.next_request().await?;
    client
        .consume(&request, |_, _, _| async {
            Ok(json!({"activity_cursor":"retained SDK cursor"}))
        })
        .await?;
    client.submit("/shell echo hi", &DeniedReader).await?;
    let request = client.next_request().await?;
    client
        .consume(&request, |_, _, _| async {
            Ok(json!({"activity_cursor":"tool-controlled text"}))
        })
        .await?;
    assert_eq!(client.activity_cursor(), Some("retained SDK cursor"));
    drop(client);
    let reopened = CommandSession::open(volume, session_id(), 32_768, 128).await?;
    assert_eq!(reopened.activity_cursor(), Some("retained SDK cursor"));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    // Every intervening publication may force an outcome retry, but the actual
    // effect is FnOnce and its immutable claim is never executed a second time.
    #[test]
    fn competing_publications_only_retry_the_outcome(count in 0_usize..8) {
        futures::executor::block_on(async {
            let volume = volume().await.unwrap();
            let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await.unwrap();
            client.submit("/shell echo hi", &DeniedReader).await.unwrap();
            let request = client.next_request().await.unwrap();
            let dispatches = std::cell::Cell::new(0);
            let result = client.consume(&request, |_, _, _| async {
                dispatches.set(dispatches.get()+1);
                for index in 0..count { publish(&volume, ".ui/config/other.json", serde_json::to_vec(&index).unwrap()).await?; }
                Ok(json!({"value":count}))
            }).await.unwrap().unwrap();
            prop_assert_eq!(dispatches.get(), 1);
            prop_assert_eq!(&client.output_page(0).unwrap().unwrap()["ok"], &result);
            prop_assert_eq!(client.output_page(0).unwrap().unwrap()["sequence"].as_u64(), Some(1));
            Ok(())
        })?;
    }

    // Dispatch authority is the exact observed immutable file, including its
    // bytes and metadata. A caller cannot replace it with a fabricated request.
    #[test]
    fn fabricated_request_cannot_reach_dispatch(text in "[a-zA-Z0-9]{1,40}") {
        futures::executor::block_on(async {
            let volume = volume().await.unwrap();
            let mut client = CommandSession::open(volume, session_id(), 32_768, 128).await.unwrap();
            client.submit("/shell echo original", &DeniedReader).await.unwrap();
            let mut fabricated = client.next_request().await.unwrap();
            fabricated.bytes = serde_json::to_vec(&json!({"kind":"tool","tool":"bash","input":{"command":text}})).unwrap().into();
            let result = client.consume(&fabricated, |_, _, _| async { panic!("fabricated input reached dispatch") }).await;
            prop_assert!(matches!(result, Err(Error::Unauthorized(_))));
            Ok(())
        })?;
    }
    // The new ordinary UI file namespace has one canonical spelling, retains
    // its caller owner through JSON, and rejects path aliases/traversal.
    #[test]
    fn ui_file_keys_round_trip(directory in "[a-zA-Z0-9_-]{1,48}", name in "[a-zA-Z0-9_-]{1,48}") {
        let key = FileKey::Ui { directory: Component::new(&directory).unwrap(), name: Component::new(format!("{name}.json")).unwrap() };
        prop_assert_eq!(FileKey::parse(&key.path()).unwrap(), key.clone());
        prop_assert_eq!(serde_json::from_slice::<FileKey>(&serde_json::to_vec(&key).unwrap()).unwrap(), key.clone());
        prop_assert_eq!(key.owner(), acyclic_harness::task_volume::Owner::Caller);
        for file in [acyclic_harness::task_volume::TaskFile::ExecutionClaim(Component::new(&name).unwrap()), acyclic_harness::task_volume::TaskFile::ExecutionResult(Component::new(&name).unwrap())] {
            let key = FileKey::Task { task:Component::new(&directory).unwrap(),file };
            prop_assert_eq!(FileKey::parse(&key.path()).unwrap(), key.clone());
            prop_assert_eq!(serde_json::from_slice::<FileKey>(&serde_json::to_vec(&key).unwrap()).unwrap(), key.clone());
            prop_assert_eq!(key.owner(), acyclic_harness::task_volume::Owner::Harness);
            let folder = format!(".tasks/{directory}/executions");
            prop_assert!(Query::folder(&folder).unwrap().matches(&key));
        }
        for bad in [format!(".ui/../{name}"), format!(".ui/{directory}//{name}"), format!(".ui/{directory}/{name}/.."), format!(".ui/{directory}/.")] {
            prop_assert!(FileKey::parse(&bad).is_err());
        }
    }
}

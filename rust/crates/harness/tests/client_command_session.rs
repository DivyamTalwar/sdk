use super::*;
use acyclic_harness::{
    OperationId, TaskId,
    client::{CommandFileRoot, CommandSession, CommandTool},
    task_volume::{Component, FileKey, FileValue, Query, TaskVolume},
};
use acyclic_stream::{MemoryStream, StreamPath};
use futures::FutureExt as _;
use proptest::prelude::*;

fn session_id() -> TaskId {
    TaskId::from_bytes([7; 16])
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
    drop(host);
    let mut config = data["config"].clone();
    let fields = json!({
        "filesystem_root":fs_root,"stream_root":directory.path().join("stream"),
        "filesystem_provider":fs_provider,"stream_provider":stream_provider,"private_volume":volume,
        "owner":{"authority":authority,"issuer":"command-client","key":vec![9_u8;32],"scope":scope},
        "generation":"commands-test","model_invocation_ledger":directory.path().join("model.jsonl"),
        "session_limits":{"active_tasks":6,"depth":3,"model_steps":7,"total_tasks":12},
        "tools":{"content_budget":data["publication"]["budget"]},"command_workspace":directory.path(),
        "deterministic_delay_ms":0
    });
    for (name, value) in fields.as_object().unwrap() {
        config[name] = value.clone();
    }
    let config_path = directory.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config)?)?;
    let mut input = tempfile::NamedTempFile::new_in(directory.path())?;
    input.write_all(b"/SH printf hi > cli-effect.txt; printf hi\n/KEYS\n/does-not-exist\n/q\n")?;
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
    assert!(
        rendered.contains("command-json=") && rendered.contains("Unknown command: /does-not-exist"),
        "{rendered}"
    );
    assert!(
        rendered.contains("/shell") && rendered.contains("/shortcuts"),
        "catalog help: {rendered}"
    );
    assert_eq!(
        std::fs::read(directory.path().join("cli-effect.txt"))?,
        b"hi"
    );
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
        ".ui/config/commands.json",
        serde_json::to_vec(&definitions).unwrap(),
    )
    .await?;
    client.observe_change().await?;
    assert_eq!(
        client.catalog().complete("EXTERNAL").next().unwrap().name,
        "shell"
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
            assert_eq!(task, session_id());
            assert_eq!(intent, resolution);
            Ok(json!({"called":true}))
        })
        .await?;
    assert_eq!(result, Some(json!({"called":true})));
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
    assert!(
        reopened
            .consume(&request, |_, _, _| async {
                panic!("uncertain dispatch was resent")
            })
            .await?
            .is_none()
    );
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    // The new ordinary UI file namespace has one canonical spelling, retains
    // its caller owner through JSON, and rejects path aliases/traversal.
    #[test]
    fn ui_file_keys_round_trip(directory in "[a-zA-Z0-9_-]{1,48}", name in "[a-zA-Z0-9_-]{1,48}") {
        let key = FileKey::Ui { directory: Component::new(&directory).unwrap(), name: Component::new(format!("{name}.json")).unwrap() };
        prop_assert_eq!(FileKey::parse(&key.path()).unwrap(), key.clone());
        prop_assert_eq!(serde_json::from_slice::<FileKey>(&serde_json::to_vec(&key).unwrap()).unwrap(), key.clone());
        prop_assert_eq!(key.owner(), acyclic_harness::task_volume::Owner::Caller);
        for bad in [format!(".ui/../{name}"), format!(".ui/{directory}//{name}"), format!(".ui/{directory}/{name}/.."), format!(".ui/{directory}/.")] {
            prop_assert!(FileKey::parse(&bad).is_err());
        }
    }
}

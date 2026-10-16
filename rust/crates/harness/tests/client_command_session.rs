use super::*;
use acyclic_harness::{
    OperationId, TaskId,
    client::{CommandFileRoot, CommandSession, CommandTool},
    task_volume::{Component, FileKey, FileValue, Query, TaskVolume},
};
use acyclic_stream::{MemoryStream, StreamPath};
use futures::FutureExt as _;
use proptest::prelude::*;
use std::sync::Arc;
#[path = "client_command_session_bounds.rs"]
mod bounds;
#[path = "client_command_activity.rs"]
mod activity;

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
    client
        .write_state(
            CommandFileRoot::Session,
            "layout.json",
            &json!({"selected_task":TaskId::from_bytes([3;16])}),
        )
        .await?;
    client
        .write_state(
            CommandFileRoot::Task,
            "settings.json",
            &json!({"agent_mode":"code"}),
        )
        .await?;
    let script = if cfg!(windows) {
        "Add-Content -NoNewline effect.txt 'hi'; Write-Output hi"
    } else {
        "printf hi >> effect.txt; printf hi"
    };
    client
        .submit(&format!("/SH {script}"), &DeniedReader)
        .await?;
    let request = client.next_request().await?;
    let settings = client.request_settings(&request).await?;
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
    assert!(reopened.take_request().is_none());
    let recovered = request.clone();
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

#[cfg(all(feature = "filesystem-local", feature = "tui"))]
#[tokio::test]
async fn file_session_shell_help_archive_and_quit_share_the_published_catalog() -> Result<()> {
    use acyclic_harness::tui::{
        ClientRenderer, PromptInput, Viewport,
        ratatui::{Terminal, backend::TestBackend},
    };
    acyclic_native_runtime::initialize_native_io_services()
        .map_err(|error| Error::Storage(error.to_string()))?;
    let fixture = Fixture::new(&[("home/.claude/commands/greet.md", "Hello $ARGUMENTS")]).await?;
    let catalog = fixture
        .catalog(&[("home", CommandScope::User)], &[])
        .await?;
    let definitions: Vec<_> = catalog.commands().cloned().collect();
    let volume = volume().await?;
    let mut files = CommandSession::open(volume.clone(), session_id(), 65_536, 128).await?;
    files
        .install_catalog(serde_json::to_vec(&definitions).unwrap())
        .await?;
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    files
        .write_state(
            CommandFileRoot::Config,
            "settings.json",
            &json!({"workspace":directory.path()}),
        )
        .await?;
    let script = if cfg!(windows) {
        "Add-Content -NoNewline cli-effect.txt 'hi'; Write-Output hi"
    } else {
        "printf hi >> cli-effect.txt; printf hi"
    };
    files
        .submit(&format!("/SH {script}"), fixture.reader.as_ref())
        .await?;
    let request = files.next_request().await?;
    let settings = files.request_settings(&request).await?;
    let result = files
        .consume(&request, |_, _, resolution| async move {
            let CommandResolution::Tool { tool, input } = resolution else {
                return Err(Error::Invalid("expected direct shell request".into()));
            };
            acyclic_harness::client::execute_host_command(tool, &input, &settings, 32_768).await
        })
        .await?
        .unwrap();
    assert_eq!(result["stdout"].as_str().unwrap().trim(), "hi");
    assert_eq!(
        std::fs::read(directory.path().join("cli-effect.txt")).unwrap(),
        b"hi"
    );
    let mut renderer = ClientRenderer::default();
    let mut previous = None;
    for _ in 0..2 {
        files.submit("/KEYS", fixture.reader.as_ref()).await?;
        let screen = renderer.screen_for(&files)?;
        let mut terminal = Terminal::new(TestBackend::new(100, 100)).unwrap();
        terminal
            .draw(|frame| {
                screen.render(
                    frame,
                    &PromptInput::default(),
                    &mut Viewport::default(),
                    0,
                    0,
                );
            })
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for name in ["/shell", "/shortcuts", "/greet"] {
            assert!(rendered.contains(name), "{name}: {rendered}");
        }
        let buffer = terminal.backend().buffer().clone();
        if let Some(previous) = previous {
            assert_eq!(buffer, previous);
        }
        previous = Some(buffer);
    }
    for _ in 0..2 {
        assert_eq!(files.output_page(0)?.unwrap()["ok"], result);
    }
    let sequence = files.output_sequence();
    assert!(
        files
            .submit("/does-not-exist", fixture.reader.as_ref())
            .await
            .is_err()
    );
    assert_eq!(files.output_sequence(), sequence);
    assert_eq!(
        std::fs::read(directory.path().join("cli-effect.txt")).unwrap(),
        b"hi"
    );
    files.submit("/q", fixture.reader.as_ref()).await?;
    assert!(files.exit_requested()?);
    let other = CommandSession::open(volume.clone(), uuid::Uuid::new_v4(), 65_536, 128).await?;
    assert!(!other.exit_requested()?);
    drop(files);
    let reopened = CommandSession::open(volume, session_id(), 65_536, 128).await?;
    assert!(reopened.exit_requested()?);
    assert_eq!(reopened.output_page(0)?.unwrap()["ok"], result);
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
    use acyclic_harness::{
        tui::{Editor, PromptInput},
        tui_files::CompletionFiles,
    };
    let volume = volume().await?;
    let mut client = CommandSession::open(volume.clone(), session_id(), 32_768, 128).await?;
    let mut input = PromptInput::new(Editor::new("/SH"));
    let mut completions = CompletionFiles::observe(&volume, &mut input, None, 128)
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;
    let candidate = input.completion.items().next().unwrap();
    assert_eq!(candidate.value, "shell");
    assert!(candidate.tool);
    assert!(input.completion.accept(&mut input.editor, 0));
    assert_eq!(input.editor.text(), "/shell ");
    publish(
        &volume,
        "commands/themes.json",
        br#"["Solarized Light"]"#.to_vec(),
    )
    .await?;
    client.observe_change().await?;
    completions
        .next(&mut input)
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;
    input.editor.set_text("/THEME sol", 1);
    input.refresh_completions();
    assert!(input.completion.accept(&mut input.editor, 2));
    assert_eq!(input.editor.text(), "/THEME Solarized Light");
    client.submit(input.editor.text(), &DeniedReader).await?;
    assert_eq!(
        client
            .state(CommandFileRoot::Config, "theme.json")?
            .unwrap()["name"],
        "Solarized Light"
    );
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
        client.request_settings(&request).await?["workspace"],
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
            let cut = volume.snapshot(&Query::Root, 128).await.unwrap();
            prop_assert_eq!(client.output_page(0).unwrap().unwrap()["sequence"].as_u64(), Some(cut.generation.position - 1));
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

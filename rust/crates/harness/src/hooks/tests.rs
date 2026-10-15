use super::*;
use serde_json::json;

#[test]
fn claude_json_and_codex_toml_are_additive_and_match_standard_events() -> Result<()> {
    let mut hooks = Hooks::default();
    hooks.add_json(br#"{"hooks":{
        "SessionStart":[{"matcher":"startup|resume","hooks":[{"type":"command","command":"start"}]}],
        "PreToolUse":[{"matcher":"^(write|edit)$","hooks":[{"type":"command","command":"check","timeout":30}]}],
        "Stop":[{"matcher":"ignored[","hooks":[{"type":"command","command":"stop"}]}]
    }}"#)?;
    hooks.add_toml(
        r#"
        [[hooks.PreToolUse]]
        matcher = "write"
        [[hooks.PreToolUse.hooks]]
        type = "command"
        command = "format"
    "#,
    )?;
    let registry = registry()?;
    let input = json!({"tool_name":"write","source":"resume"});
    assert_eq!(
        hooks
            .commands(HookEvent::SessionStart, &input, &registry)?
            .count(),
        1
    );
    assert_eq!(
        hooks
            .commands(HookEvent::PreToolUse, &input, &registry)?
            .map(|hook| hook.command.as_str())
            .collect::<Vec<_>>(),
        ["check", "format"]
    );
    assert_eq!(
        hooks.commands(HookEvent::Stop, &input, &registry)?.count(),
        1
    );
    assert_eq!(
        hooks
            .commands(
                HookEvent::PreToolUse,
                &json!({"tool_name":"read"}),
                &registry
            )?
            .count(),
        0
    );
    assert_eq!(
        hooks
            .commands(HookEvent::PostToolUse, &input, &registry)?
            .count(),
        0
    );
    Ok(())
}

#[test]
fn invalid_hook_replacement_is_atomic() -> Result<()> {
    let mut hooks = Hooks::default();
    for document in [
        json!({"hooks":{"PreToolUse":[{"matcher":"[","hooks":[]}]}}),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":""}]}]}}),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"test","timeout":0}]}]}}),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"prompt","command":"test"}]}]}}),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"test","async":true}]}]}}),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"test","shell":"invalid"}]}]}}),
    ] {
        assert!(hooks.add_json(document.to_string().as_bytes()).is_err());
        assert!(hooks.bindings.is_empty());
    }
    Ok(())
}

fn registry() -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::default();
    let tools: &[(&str, &'static [&'static str])] = &[
        ("bash", &["Bash"]),
        ("edit", &["Edit"]),
        ("write", &["Write"]),
        ("read", &["Read"]),
        ("fs", &[]),
        ("NotebookEdit", &[]),
    ];
    for &(name, names) in tools {
        let mut tool = crate::tool::files::read_file()?;
        tool.definition.name = name.into();
        tool.hook_names = names;
        registry.register((tool, None), &mut None, None)?;
    }
    Ok(registry)
}

#[test]
fn tool_matchers_use_declared_names_and_exact_identifiers() -> Result<()> {
    let registry = registry()?;
    let mut hooks = Hooks::default();
    hooks.add_json(
        br#"{"hooks":{"PreToolUse":[
        {"matcher":"Bash","hooks":[{"type":"command","command":"guard"}]},
        {"matcher":"Edit","hooks":[{"type":"command","command":"edit"}]},
        {"matcher":"Edit|Write","hooks":[{"type":"command","command":"changes"}]},
        {"matcher":"fs","hooks":[{"type":"command","command":"fs"}]}
    ]}}"#,
    )?;
    for (name, commands) in [
        ("bash", vec!["guard"]),
        ("edit", vec!["edit", "changes"]),
        ("write", vec!["changes"]),
        ("fs", vec!["fs"]),
        // Regex alternation keeps regex semantics; plain Edit is anchored.
        ("NotebookEdit", vec!["changes"]),
        ("read", vec![]),
    ] {
        assert_eq!(
            hooks
                .commands(HookEvent::PreToolUse, &json!({"tool_name":name}), &registry)?
                .map(|hook| hook.command.as_str())
                .collect::<Vec<_>>(),
            commands
        );
    }
    Ok(())
}

#[test]
fn non_blocking_failures_return_diagnostics() -> Result<()> {
    let timeout = Error::process_io(std::io::ErrorKind::TimedOut.into());
    assert_eq!(timeout.code(), crate::wire::ErrorCode::Invalid);
    assert!(matches!(
        timeout,
        Error::Process {
            kind: "timeout",
            ..
        }
    ));
    for event in [
        HookEvent::SessionStart,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::Stop,
    ] {
        for code in [Some(1), Some(3), None] {
            let mut output = HookOutput::default();
            output.observe(event, code, b"ignored", b"command failed")?;
            assert!(output.blocked.is_none());
            assert_eq!(output.diagnostics.len(), 1);
            assert!(output.diagnostics[0].contains(event.name()));
            assert!(output.diagnostics[0].contains("command failed"));
        }
    }
    let mut output = HookOutput::default();
    output.observe(HookEvent::SessionStart, Some(2), b"", b"start failed")?;
    assert!(output.diagnostics[0].contains("start failed"));
    Ok(())
}

#[test]
fn output_applies_context_tool_decisions_and_stop_blocking() -> Result<()> {
    let mut output = HookOutput::default();
    output.observe(HookEvent::SessionStart, Some(0), b"workspace ready\n", b"")?;
    assert_eq!(output.additional_context, ["workspace ready"]);
    output.observe(
        HookEvent::PreToolUse,
        Some(0),
        br#"{"hookSpecificOutput":{
        "hookEventName":"PreToolUse","updatedInput":{"path":"src/lib.rs"},
        "additionalContext":"Inspect tests first"
    }}"#,
        b"",
    )?;
    assert_eq!(output.updated_input, Some(json!({"path":"src/lib.rs"})));
    assert_eq!(
        output.additional_context,
        ["workspace ready", "Inspect tests first"]
    );
    output.observe(
        HookEvent::PreToolUse,
        Some(0),
        br#"{"hookSpecificOutput":{
        "permissionDecision":"deny","permissionDecisionReason":"protected path"
    }}"#,
        b"",
    )?;
    assert_eq!(output.blocked.as_deref(), Some("protected path"));
    let mut output = HookOutput::default();
    output.observe(HookEvent::Stop, Some(2), b"", b"tests failed")?;
    assert_eq!(output.blocked.as_deref(), Some("tests failed"));
    let mut post = HookOutput::default();
    post.observe(HookEvent::PostToolUse, Some(2), b"", b"format failed")?;
    assert_eq!(post.additional_context, ["format failed"]);
    assert!(post.blocked.is_none());
    assert!(
        output
            .observe(
                HookEvent::PreToolUse,
                Some(0),
                br#"{"hookSpecificOutput":{"hookEventName":"Stop"}}"#,
                b""
            )
            .is_err()
    );
    assert!(
        output
            .observe(
                HookEvent::PreToolUse,
                Some(0),
                br#"{"hookSpecificOutput":{"updatedInput":[]}}"#,
                b""
            )
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn matching_hooks_run_concurrently_and_merge_in_config_order() -> Result<()> {
    use futures::{FutureExt as _, channel::oneshot};
    use std::cell::Cell;
    let mut hooks = Hooks::default();
    hooks.add_json(
        br#"{"hooks":{"PreToolUse":[{"hooks":[
        {"type":"command","command":"first"},
        {"type":"command","command":"second"}
    ]}]}}"#,
    )?;
    let registry = registry()?;
    let input = json!({"tool_name":"bash"});
    let started = Cell::new(0);
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let mut receivers = [first_rx, second_rx].into_iter();
    let future = hooks.run_commands(HookEvent::PreToolUse, &input, &registry, |_| {
        let receiver = receivers.next();
        let started = &started;
        async move {
            started.set(started.get() + 1);
            Ok((
                Some(0),
                receiver
                    .ok_or_else(|| Error::Invalid("missing hook receiver".into()))?
                    .await
                    .map_err(|error| Error::Invalid(error.to_string()))?,
                vec![],
            ))
        }
    });
    futures::pin_mut!(future);
    assert!(future.as_mut().now_or_never().is_none());
    assert_eq!(
        started.get(),
        2,
        "both commands start before either completes"
    );
    let response = |name| {
        serde_json::to_vec(&json!({"decision":"block","reason":name,
        "hookSpecificOutput":{"additionalContext":name,"updatedInput":{"winner":name}}}))
        .map_err(|error| Error::Invalid(error.to_string()))
    };
    second_tx
        .send(response("second")?)
        .map_err(|_| Error::Invalid("second hook cancelled".into()))?;
    first_tx
        .send(response("first")?)
        .map_err(|_| Error::Invalid("first hook cancelled".into()))?;
    let output = future.await?;
    assert_eq!(output.blocked.as_deref(), Some("first"));
    assert_eq!(output.additional_context, ["first", "second"]);
    assert_eq!(output.updated_input, Some(json!({"winner":"second"})));
    Ok(())
}

#[tokio::test]
async fn process_failures_and_invalid_outputs_are_diagnostics_and_do_not_stop_other_hooks()
-> Result<()> {
    let mut hooks = Hooks::default();
    hooks.add_json(
        br#"{"hooks":{"SessionStart":[{"hooks":[
        {"type":"command","command":"timeout"},
        {"type":"command","command":"spawn"},
        {"type":"command","command":"invalid"},
        {"type":"command","command":"success"}
    ]}]}}"#,
    )?;
    let output = hooks
        .run_commands(HookEvent::SessionStart, &json!({}), &registry()?, |hook| {
            let result = match hook.command.as_str() {
                "timeout" => Err(Error::Process {
                    kind: "timeout",
                    message: "deadline elapsed".into(),
                }),
                "spawn" => Err(Error::Process {
                    kind: "not_found",
                    message: "shell missing".into(),
                }),
                "invalid" => Ok((
                    Some(0),
                    br#"{"hookSpecificOutput":{"hookEventName":"Stop"}}"#.to_vec(),
                    vec![],
                )),
                _ => Ok((Some(0), b"ready".to_vec(), vec![])),
            };
            std::future::ready(result)
        })
        .await?;
    assert!(output.blocked.is_none());
    assert_eq!(output.additional_context, ["ready"]);
    assert_eq!(output.diagnostics.len(), 3);
    assert!(output.diagnostics[0].contains("timeout"));
    assert!(output.diagnostics[1].contains("shell missing"));
    assert!(output.diagnostics[2].contains("another event"));
    Ok(())
}

#[tokio::test]
async fn native_hooks_default_user_guards_and_project_trust_use_one_shared_script() -> Result<()> {
    let process_error = Error::process_io;
    let fixture = tempfile::tempdir().map_err(process_error)?;
    let home = fixture.path().join("home");
    let workspace = fixture.path().join("project");
    let cwd = workspace.join("nested");
    std::fs::create_dir_all(&cwd).map_err(process_error)?;
    #[cfg(windows)]
    let (script_name, script, shell) = (
        "fixture.cmd",
        r#"@echo off
setlocal
set /p payload=
if not defined payload exit /b 3
if not "%CD%"=="%CLAUDE_PROJECT_DIR%\nested" exit /b 4
> "%CLAUDE_PROJECT_DIR%\hook-ran" echo %payload%
exit /b 2
"#,
        "cmd",
    );
    #[cfg(not(windows))]
    let (script_name, script, shell) = (
        "fixture.sh",
        r#"IFS= read -r input
test -n "$input" || exit 3
test "$(pwd -P)" = "$(cd "$CLAUDE_PROJECT_DIR/nested" && pwd -P)" || exit 4
printf '%s' "$input" > "$CLAUDE_PROJECT_DIR/hook-ran"
exit 2
"#,
        "sh",
    );
    let script_path = fixture.path().join(script_name);
    std::fs::write(&script_path, script).map_err(process_error)?;
    #[cfg(windows)]
    let command = format!("call \"{}\"", script_path.display());
    #[cfg(not(windows))]
    let command = format!("sh \"{}\"", script_path.display());
    let hook = json!({"type":"command","command":command,"shell":shell});
    let write = |root: &std::path::Path, path: &str, value: &Value| -> Result<()> {
        let path = root.join(path);
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| Error::Invalid("fixture parent missing".into()))?,
        )
        .map_err(process_error)?;
        std::fs::write(
            path,
            serde_json::to_vec(value).map_err(|error| Error::Invalid(error.to_string()))?,
        )
        .map_err(process_error)
    };
    write(
        &home,
        ".claude/settings.json",
        &json!({"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[hook]}]}}),
    )?;
    let project = json!({"hooks":{"Stop":[{"hooks":[hook]}]}});
    for path in [
        ".claude/settings.json",
        ".claude/settings.local.json",
        ".codex/hooks.json",
    ] {
        write(&workspace, path, &project)?;
    }
    std::fs::write(workspace.join(".codex/config.toml"), format!(
        "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = 'command'\ncommand = {command:?}\nshell = '{shell}'\n"
    )).map_err(process_error)?;
    let registry = registry()?;
    let hooks = Hooks::load(HooksConfig::new(&workspace, &home)).await?;
    let input = json!({"tool_name":"bash","session_id":"test"});
    assert_eq!(
        hooks
            .run(HookEvent::Stop, &workspace, &cwd, input.clone(), &registry)
            .await?,
        HookOutput::default()
    );
    assert!(
        !workspace.join("hook-ran").exists(),
        "untrusted project commands never execute"
    );
    let output = hooks
        .run(
            HookEvent::PreToolUse,
            &workspace,
            &cwd,
            input.clone(),
            &registry,
        )
        .await?;
    assert!(
        output.blocked.is_some(),
        "user Bash guard fires on registry tool bash: {output:?}"
    );
    assert!(output.diagnostics.is_empty(), "{output:?}");
    assert!(
        workspace.join("hook-ran").exists(),
        "script receives the project root and per-call cwd"
    );
    let captured: Value =
        serde_json::from_slice(&std::fs::read(workspace.join("hook-ran")).map_err(process_error)?)
            .map_err(|error| Error::Invalid(error.to_string()))?;
    assert_eq!(
        captured,
        json!({"tool_name":"bash", "session_id":"test", "hook_event_name":"PreToolUse", "cwd":cwd.to_string_lossy()})
    );
    let failure = hooks
        .run(
            HookEvent::PreToolUse,
            &workspace,
            &workspace.join("missing"),
            input,
            &registry,
        )
        .await?;
    assert!(failure.blocked.is_none());
    assert_eq!(failure.diagnostics.len(), 1);
    let trusted =
        Hooks::load(HooksConfig::new(&workspace, &home).trust_project_hooks(true)).await?;
    assert_eq!(
        trusted
            .commands(HookEvent::Stop, &json!({}), &registry)?
            .count(),
        4
    );
    let trusted_output = trusted
        .run_commands(HookEvent::Stop, &json!({}), &registry, |_| {
            std::future::ready(Ok((Some(2), vec![], b"project guard".to_vec())))
        })
        .await?;
    assert_eq!(trusted_output.blocked.as_deref(), Some("project guard"));
    std::fs::remove_file(home.join(".claude/settings.json")).map_err(process_error)?;
    std::fs::create_dir(home.join(".claude/settings.json")).map_err(process_error)?;
    assert!(matches!(
        Hooks::load(HooksConfig::new(&workspace, &home)).await,
        Err(Error::Process { .. })
    ));
    Ok(())
}

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
    let input = json!({"tool_name":"write","source":"resume"});
    assert_eq!(hooks.commands(HookEvent::SessionStart, &input).count(), 1);
    assert_eq!(
        hooks
            .commands(HookEvent::PreToolUse, &input)
            .map(|hook| hook.command.as_str())
            .collect::<Vec<_>>(),
        ["check", "format"]
    );
    assert_eq!(hooks.commands(HookEvent::Stop, &input).count(), 1);
    assert_eq!(
        hooks
            .commands(HookEvent::PreToolUse, &json!({"tool_name":"read"}))
            .count(),
        0
    );
    assert_eq!(hooks.commands(HookEvent::PostToolUse, &input).count(), 0);
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

#[test]
fn tool_matchers_use_registry_names_without_builtin_aliases() -> Result<()> {
    let mut hooks = Hooks::default();
    hooks.add_json(
        br#"{"hooks":{"PreToolUse":[
        {"matcher":"^(fs|custom.tool)$","hooks":[{"type":"command","command":"exact"}]},
        {"matcher":"^Bash$","hooks":[{"type":"command","command":"case-sensitive"}]}
    ]}}"#,
    )?;
    for name in ["fs", "custom.tool", "Bash"] {
        assert_eq!(
            hooks
                .commands(HookEvent::PreToolUse, &json!({"tool_name":name}))
                .count(),
            1
        );
    }
    assert_eq!(
        hooks
            .commands(HookEvent::PreToolUse, &json!({"tool_name":"bash"}))
            .count(),
        0
    );
    Ok(())
}

#[test]
fn non_blocking_failures_return_diagnostics() -> Result<()> {
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

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn native_hooks_load_layers_and_receive_json_stdin_in_workspace() -> Result<()> {
    let home = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let workspace = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let write = |root: &std::path::Path, path: &str, value: Value| -> Result<()> {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| Error::Storage(error.to_string()))?;
        }
        std::fs::write(
            path,
            serde_json::to_vec(&value).map_err(|error| Error::Invalid(error.to_string()))?,
        )
        .map_err(|error| Error::Storage(error.to_string()))
    };
    #[cfg(windows)]
    let command = "$data = [Console]::In.ReadToEnd() | ConvertFrom-Json; if ($data.session_id -ne 'test' -or $data.hook_event_name -ne 'SessionStart' -or $data.cwd -ne (Get-Location).Path -or $env:CLAUDE_PROJECT_DIR -ne $data.cwd) { exit 2 }; 'hook ready'";
    #[cfg(not(windows))]
    let command = "read input; test \"$(pwd -P)\" = \"$(cd \"$CLAUDE_PROJECT_DIR\" && pwd -P)\" || exit 2; case \"$input\" in *test*SessionStart*|*SessionStart*test*) printf 'hook ready';; *) exit 2;; esac";
    write(
        home.path(),
        ".claude/settings.json",
        json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":command}]}]}}),
    )?;
    #[cfg(windows)]
    let project_command =
        "[IO.File]::WriteAllText((Join-Path (Get-Location) 'project-hook-ran'), 'ran'); exit 2";
    #[cfg(not(windows))]
    let project_command = "printf ran > project-hook-ran; exit 2";
    let project_hook =
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":project_command}]}]}});
    for path in [
        ".claude/settings.json",
        ".claude/settings.local.json",
        ".codex/hooks.json",
    ] {
        write(workspace.path(), path, project_hook.clone())?;
    }
    std::fs::write(workspace.path().join(".codex/config.toml"), format!(
        "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = 'command'\ncommand = {project_command:?}\n"
    )).map_err(Error::Process)?;
    let hooks = Hooks::load(HooksConfig::new(workspace.path(), home.path())).await?;
    let output = hooks
        .run(
            HookEvent::SessionStart,
            workspace.path(),
            json!({"session_id":"test","source":"startup"}),
        )
        .await?;
    assert_eq!(output.additional_context, ["hook ready"]);
    let input = json!({"session_id":"test"});
    assert_eq!(
        hooks
            .run(HookEvent::Stop, workspace.path(), input.clone())
            .await?,
        HookOutput::default()
    );
    assert!(!workspace.path().join("project-hook-ran").exists());
    assert!(matches!(
        hooks
            .run(
                HookEvent::SessionStart,
                &workspace.path().join("missing"),
                input
            )
            .await,
        Err(Error::Process(_))
    ));
    let hooks =
        Hooks::load(HooksConfig::new(workspace.path(), home.path()).trust_project_hooks(true))
            .await?;
    assert!(
        hooks
            .run(
                HookEvent::Stop,
                workspace.path(),
                json!({"session_id":"test"})
            )
            .await?
            .blocked
            .is_some()
    );
    assert!(workspace.path().join("project-hook-ran").exists());
    // A directory in a user config location is a real IO failure, not storage failure.
    std::fs::remove_file(home.path().join(".claude/settings.json")).map_err(Error::Process)?;
    std::fs::create_dir(home.path().join(".claude/settings.json")).map_err(Error::Process)?;
    assert!(matches!(
        Hooks::load(HooksConfig::new(workspace.path(), home.path())).await,
        Err(Error::Process(_))
    ));
    Ok(())
}

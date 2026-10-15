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
    write(
        workspace.path(),
        ".codex/hooks.json",
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"exit 2"}]}]}}),
    )?;
    let hooks = Hooks::load(workspace.path(), home.path()).await?;
    let output = hooks
        .run(
            HookEvent::SessionStart,
            workspace.path(),
            json!({"session_id":"test","source":"startup"}),
        )
        .await?;
    assert_eq!(output.additional_context, ["hook ready"]);
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
    Ok(())
}

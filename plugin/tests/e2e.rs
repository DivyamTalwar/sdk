//! Native hook processes are clients, and cannot create a second task owner.
use acyclic_harness::external_executor::{ExternalHost, hooks};
use serde::Deserialize;
use serde_json::Value;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
struct Scenario {
    host: ExternalHost,
    event: String,
    input: Value,
}

#[test]
fn missing_binding_fails_closed_without_starting_a_plugin_host() -> Result {
    let directory = tempfile::tempdir()?;
    let scenarios: Vec<Scenario> = serde_json::from_str(include_str!(
        "../../rust/crates/harness/tests/external_hooks.json"
    ))?;
    for scenario in scenarios {
        let mut child = Command::new(env!("CARGO_BIN_EXE_acyclic"))
            .args(["__hook", scenario.host.name(), &scenario.event])
            .current_dir(directory.path())
            .env_remove("ACYCLIC_HARNESS_BINDING")
            .env("HOME", directory.path())
            .env("USERPROFILE", directory.path())
            .env("XDG_STATE_HOME", directory.path())
            .env("LOCALAPPDATA", directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("missing hook stdin")?
            .write_all(&serde_json::to_vec(&scenario.input)?)?;
        let output = child.wait_with_output()?;
        assert_eq!(output.status.code(), Some(2));
        let answer: Value = serde_json::from_slice(&output.stdout)?;
        if scenario.event == "PreToolUse" {
            assert_eq!(
                answer.pointer("/hookSpecificOutput/permissionDecision"),
                Some(&Value::String("deny".into()))
            );
        } else if scenario.event == "tool_call" {
            assert_eq!(answer.get("block"), Some(&Value::Bool(true)));
        } else {
            assert!(
                answer
                    .get("systemMessage")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("ACYCLIC_HARNESS_BINDING"))
            );
        }
        assert_eq!(
            std::fs::read_dir(directory.path())?.count(),
            0,
            "a hook may not initialize an independent host"
        );
    }
    Ok(())
}

#[test]
fn codex_and_pi_manifests_route_every_event_to_the_common_client() -> Result {
    let codex: Value = serde_json::from_str(include_str!("../hooks/hooks.json"))?;
    let pi: Value = serde_json::from_str(include_str!("../pi/package.json"))?;
    for spec in hooks(ExternalHost::Codex) {
        let hook = &codex["hooks"][spec.event][0]["hooks"][0];
        assert_eq!(hook["type"], "command");
        assert!(
            hook["command"]
                .as_str()
                .is_some_and(|command| command.ends_with(&format!("__hook codex {}", spec.event)))
        );
        assert_eq!(hook["timeout"].as_u64(), Some(spec.timeout_seconds()));
    }
    for spec in hooks(ExternalHost::Pi) {
        assert_eq!(
            pi["acyclic"]["hooks"][spec.event]["args"],
            serde_json::json!(["__hook", "pi", spec.event])
        );
    }
    assert!(
        !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".mcp.json")
            .exists()
    );
    for file in ["../src/main.rs", "../pi/index.ts"] {
        let source = std::fs::read_to_string(
            std::path::Path::new(file)
                .strip_prefix("../")
                .map(|path| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))?,
        )?;
        assert!(source.lines().count() <= 300);
        for old in [
            "ConcurrentServiceControl",
            "ControlEndpoint",
            "thread_stack_size",
            "setTimeout",
            "while (true)",
        ] {
            assert!(!source.contains(old), "retired owner in {file}: {old}");
        }
    }
    Ok(())
}

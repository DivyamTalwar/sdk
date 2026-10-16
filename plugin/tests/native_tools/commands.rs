//! Probe the installed CLI, not a manually constructed task context.
use super::*;

#[tokio::test]
async fn native_shell_and_git_commands_use_the_fenced_owner() -> Result {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join(".git"))?;
    let session = uuid::Uuid::new_v4().to_string();
    let cwd = directory.path();
    hook(cwd, "pi", "session_start", json!({"sessionId":session,"cwd":cwd})).await?;
    for (name, command) in [
        ("bash", "printf fenced-shell > shell.txt"),
        ("fs", "status"),
    ] {
        let answer = hook(cwd, "pi", "tool_call", json!({
            "sessionId":session,"cwd":cwd,"toolCallId":name,
            "toolName":name,"input":{"command":command},
        })).await?;
        assert_eq!(answer["isError"], false, "{name}: {answer}");
        assert!(answer["details"].is_object(), "{name}: {answer}");
    }
    assert_eq!(std::fs::read_to_string(cwd.join("shell.txt"))?, "fenced-shell");
    hook(cwd, "pi", "session_shutdown", json!({"sessionId":session,"cwd":cwd})).await?;
    Ok(())
}

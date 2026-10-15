use super::*;
use acyclic_native_runtime::{CaptureInput, ProcessInput, ProcessTree, run_blocking_io};
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

impl Hooks {
    /// Loads additive user and project hooks from Claude and Codex config locations.
    pub async fn load(workspace: &Path, home: &Path) -> Result<Self> {
        acyclic_native_runtime::initialize_native_io_services().map_err(storage)?;
        let paths = [
            (home.join(".claude/settings.json"), false),
            (home.join(".codex/hooks.json"), false),
            (home.join(".codex/config.toml"), true),
            (workspace.join(".claude/settings.json"), false),
            (workspace.join(".claude/settings.local.json"), false),
            (workspace.join(".codex/hooks.json"), false),
            (workspace.join(".codex/config.toml"), true),
        ];
        run_blocking_io(
            move || {
                let mut hooks = Self::default();
                for (path, toml) in paths {
                    let bytes = match std::fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(Error::Storage(format!("{}: {error}", path.display())));
                        }
                    };
                    if toml {
                        hooks.add_toml(
                            std::str::from_utf8(&bytes)
                                .map_err(|error| Error::Invalid(error.to_string()))?,
                        )?;
                    } else {
                        hooks.add_json(&bytes)?;
                    }
                }
                Ok(hooks)
            },
            None,
        )
        .map_err(storage)?
        .await
        .map_err(storage)?
    }

    /// Runs matching hooks with JSON stdin, bounded output, deadlines, and tree cleanup.
    pub async fn run(&self, event: HookEvent, cwd: &Path, mut input: Value) -> Result<HookOutput> {
        if self.bindings.is_empty() {
            return Ok(HookOutput::default());
        }
        let fields = input
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("hook input must be an object".into()))?;
        fields.insert("hook_event_name".into(), Value::String(event.name().into()));
        fields.insert(
            "cwd".into(),
            Value::String(cwd.to_string_lossy().into_owned()),
        );
        let mut output = HookOutput::default();
        let mut commands = self.commands(event, &input).peekable();
        if commands.peek().is_none() {
            return Ok(output);
        }
        acyclic_native_runtime::initialize_native_io_services().map_err(storage)?;
        let mut bytes =
            serde_json::to_vec(&input).map_err(|error| Error::Invalid(error.to_string()))?;
        bytes.push(b'\n');
        let request = Arc::new((cwd.to_path_buf(), bytes));
        for hook in commands {
            let hook = Arc::clone(hook);
            let request = Arc::clone(&request);
            let result = run_blocking_io(
                move || {
                    let mut command = shell(&hook.command, hook.shell.as_deref());
                    command
                        .current_dir(&request.0)
                        .env("CLAUDE_PROJECT_DIR", &request.0)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped());
                    let mut tree = ProcessTree::spawn(&mut command).map_err(storage)?;
                    tree.wait_with_output(
                        Duration::from_secs(hook.timeout),
                        65_536,
                        Some(CaptureInput::Literal(&request.1)),
                        || Ok(()),
                        |_| Ok(ProcessInput::default()),
                        None,
                        None,
                    )
                    .map_err(|failure| storage(failure.error))
                },
                None,
            )
            .map_err(storage)?
            .await
            .map_err(storage)??;
            output.observe(event, result.status.code(), &result.stdout, &result.stderr)?;
            if output.blocked.is_some() {
                break;
            }
        }
        Ok(output)
    }
}

fn storage(error: std::io::Error) -> Error {
    Error::Storage(error.to_string())
}

fn shell(command: &str, selected: Option<&str>) -> Command {
    let selected = selected.unwrap_or(if cfg!(windows) { "powershell" } else { "sh" });
    let mut shell = Command::new(selected);
    if matches!(selected, "powershell" | "pwsh") {
        shell.args(["-NoProfile", "-NonInteractive", "-Command", command]);
    } else {
        shell.args(["-c", command]);
    }
    shell
}

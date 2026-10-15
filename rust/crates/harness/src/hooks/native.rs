use super::*;
use acyclic_native_runtime::{
    CaptureInput, ProcessInput, ProcessTree, run_blocking_io, shell_command,
};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

/// Native hook locations and explicit authority to execute project commands.
pub struct HooksConfig {
    workspace: PathBuf,
    home: PathBuf,
    trust_project_hooks: bool,
}

impl HooksConfig {
    /// Loads user configuration by default. Project configuration is untrusted.
    pub fn new(workspace: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            home: home.into(),
            trust_project_hooks: false,
        }
    }

    /// Explicitly permits commands supplied by the workspace's configuration.
    #[must_use]
    pub fn trust_project_hooks(mut self, trust: bool) -> Self {
        self.trust_project_hooks = trust;
        self
    }
}

impl Hooks {
    /// Loads user hooks, and project hooks only with explicit trust in `config`.
    pub async fn load(config: HooksConfig) -> Result<Self> {
        acyclic_native_runtime::initialize_native_io_services().map_err(Error::Process)?;
        run_blocking_io(
            move || {
                let mut hooks = Self::default();
                let user = [
                    ".claude/settings.json",
                    ".codex/hooks.json",
                    ".codex/config.toml",
                ];
                let project = [
                    ".claude/settings.json",
                    ".claude/settings.local.json",
                    ".codex/hooks.json",
                    ".codex/config.toml",
                ];
                let paths = user.into_iter().map(|path| config.home.join(path)).chain(
                    project
                        .into_iter()
                        .filter(|_| config.trust_project_hooks)
                        .map(|path| config.workspace.join(path)),
                );
                for path in paths {
                    let bytes = match std::fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(Error::Process(error));
                        }
                    };
                    if path
                        .extension()
                        .is_some_and(|extension| extension == "toml")
                    {
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
        .map_err(Error::Process)?
        .await
        .map_err(Error::Process)?
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
        acyclic_native_runtime::initialize_native_io_services().map_err(Error::Process)?;
        let mut bytes =
            serde_json::to_vec(&input).map_err(|error| Error::Invalid(error.to_string()))?;
        bytes.push(b'\n');
        let request = Arc::new((cwd.to_path_buf(), bytes));
        for hook in commands {
            let hook = Arc::clone(hook);
            let request = Arc::clone(&request);
            let result = run_blocking_io(
                move || {
                    let mut command = shell_command(&hook.command, hook.shell.as_deref());
                    command
                        .current_dir(&request.0)
                        .env("CLAUDE_PROJECT_DIR", &request.0)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped());
                    let mut tree = ProcessTree::spawn(&mut command).map_err(Error::Process)?;
                    tree.wait_with_output(
                        Duration::from_secs(hook.timeout),
                        65_536,
                        Some(CaptureInput::Literal(&request.1)),
                        || Ok(()),
                        |_| Ok(ProcessInput::default()),
                        None,
                        None,
                    )
                    .map_err(|failure| Error::Process(failure.error))
                },
                None,
            )
            .map_err(Error::Process)?
            .await
            .map_err(Error::Process)??;
            output.observe(event, result.status.code(), &result.stdout, &result.stderr)?;
            if output.blocked.is_some() {
                break;
            }
        }
        Ok(output)
    }
}

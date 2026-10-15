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
        acyclic_native_runtime::initialize_native_io_services().map_err(Error::process_io)?;
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
                            return Err(Error::process_io(error));
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
        .map_err(Error::process_io)?
        .await
        .map_err(Error::process_io)?
    }

    /// Runs matching hooks concurrently, merging decisions in configuration order.
    /// `workspace` is the project root; `cwd` is the individual tool's directory.
    pub async fn run(
        &self,
        event: HookEvent,
        workspace: &Path,
        cwd: &Path,
        mut input: Value,
        registry: &ToolRegistry,
    ) -> Result<HookOutput> {
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
        let mut bytes =
            serde_json::to_vec(&input).map_err(|error| Error::Invalid(error.to_string()))?;
        bytes.push(b'\n');
        let request = Arc::new((workspace.to_path_buf(), cwd.to_path_buf(), bytes));
        self.run_commands(event, &input, registry, |hook| {
            run_hook(Arc::clone(hook), Arc::clone(&request))
        })
        .await
    }
}

async fn run_hook(
    hook: Arc<CommandHook>,
    request: Arc<(PathBuf, PathBuf, Vec<u8>)>,
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>)> {
    acyclic_native_runtime::initialize_native_io_services().map_err(Error::process_io)?;
    run_blocking_io(
        move || {
            let mut command = shell_command(&hook.command, hook.shell.as_deref());
            command
                .current_dir(&request.1)
                .env("CLAUDE_PROJECT_DIR", &request.0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut tree = ProcessTree::spawn(&mut command).map_err(Error::process_io)?;
            let output = tree
                .wait_with_output(
                    Duration::from_secs(hook.timeout),
                    65_536,
                    Some(CaptureInput::Literal(&request.2)),
                    || Ok(()),
                    |_| Ok(ProcessInput::default()),
                    None,
                    None,
                )
                .map_err(|failure| Error::process_io(failure.error))?;
            Ok((output.status.code(), output.stdout, output.stderr))
        },
        None,
    )
    .map_err(Error::process_io)?
    .await
    .map_err(Error::process_io)?
}

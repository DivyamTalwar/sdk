use super::*;
use acyclic_native_runtime::{
    CaptureInput, ProcessInput, ProcessTree, run_blocking_io, shell_command,
};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Native hook locations and explicit authority to execute project commands.
pub struct HooksConfig {
    workspace: PathBuf,
    home: PathBuf,
    codex_home: PathBuf,
    trust_project_hooks: bool,
}

impl HooksConfig {
    /// Loads user configuration by default. Project configuration is untrusted.
    pub fn new(workspace: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let codex_home = codex_directory(&home, std::env::var_os("CODEX_HOME").as_deref());
        Self {
            workspace: workspace.into(),
            home,
            codex_home,
            trust_project_hooks: false,
        }
    }

    /// Selects an explicit Codex directory instead of CODEX_HOME or ~/.codex.
    #[must_use]
    pub fn codex_home(mut self, directory: impl Into<PathBuf>) -> Self {
        self.codex_home = directory.into();
        self
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
                    config.home.join(".claude/settings.json"),
                    config.codex_home.join("hooks.json"),
                    config.codex_home.join("config.toml"),
                ];
                let project = [
                    ".claude/settings.json",
                    ".claude/settings.local.json",
                    ".codex/hooks.json",
                    ".codex/config.toml",
                ];
                let paths = user.into_iter().chain(
                    project
                        .into_iter()
                        .filter(|_| config.trust_project_hooks)
                        .map(|path| config.workspace.join(path)),
                );
                let mut seen = std::collections::BTreeSet::new();
                for path in paths {
                    let toml = path
                        .extension()
                        .is_some_and(|extension| extension == "toml");
                    let path = match std::fs::canonicalize(path) {
                        Ok(path) => path,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(Error::process_io(error)),
                    };
                    if !seen.insert(path.clone()) {
                        continue;
                    }
                    let bytes = match std::fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(Error::process_io(error));
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

pub(super) async fn run_hook(
    hook: Arc<HookSpec>,
    request: Arc<(PathBuf, PathBuf, Vec<u8>)>,
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>)> {
    acyclic_native_runtime::initialize_native_io_services().map_err(Error::process_io)?;
    let cancellation = HookCancellation::new();
    let cancelled = Arc::clone(&cancellation.0);
    run_blocking_io(
        move || {
            let check = || check_cancelled(&cancelled);
            check().map_err(Error::process_io)?;
            let mut command = hook_command(&hook);
            command
                .current_dir(&request.1)
                .env("CLAUDE_PROJECT_DIR", &request.0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            check().map_err(Error::process_io)?;
            let mut tree = ProcessTree::spawn(&mut command).map_err(Error::process_io)?;
            let output = tree
                .wait_with_output(
                    Duration::from_millis(hook.timeout_ms),
                    65_536,
                    Some(CaptureInput::Literal(&request.2)),
                    check,
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

fn codex_directory(home: &Path, configured: Option<&std::ffi::OsStr>) -> PathBuf {
    configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

fn hook_command(hook: &HookSpec) -> Command {
    match &hook.run {
        HookRun::Executable { executable, argv } => {
            let mut command = Command::new(executable);
            command.args(argv);
            command
        }
        HookRun::Shell { command, shell } => shell_command(command, shell.as_deref()),
    }
}

struct HookCancellation(Arc<AtomicBool>);

impl HookCancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Drop for HookCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> std::io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(std::io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_home_uses_the_configured_environment_path() {
        let home = Path::new("home");
        assert_eq!(
            codex_directory(home, Some(std::ffi::OsStr::new("custom"))),
            Path::new("custom")
        );
        assert_eq!(codex_directory(home, None), home.join(".codex"));
        assert_eq!(
            codex_directory(home, Some(std::ffi::OsStr::new(""))),
            home.join(".codex")
        );
    }
}

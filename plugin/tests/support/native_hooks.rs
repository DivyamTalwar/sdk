//! Real hook-process fixture shared by focused integration suites.
use serde_json::Value;
use std::{path::Path, process::Stdio};
use tokio::{io::AsyncWriteExt as _, process::Command};
pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Fixture {
    _root: tempfile::TempDir,
    project: std::path::PathBuf,
}

impl Fixture {
    pub fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let project = root.path().join("project");
        std::fs::create_dir(&project)?;
        Ok(Self {
            _root: root,
            project,
        })
    }
    pub fn path(&self) -> &Path {
        &self.project
    }
    pub async fn close(&self) -> Result {
        let output = command(self.path()).arg("__owner-stop").output().await?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

pub fn command(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_acyclic"));
    command
        .current_dir(directory)
        .env_remove("ACYCLIC_HARNESS_BINDING")
        .env(
            "ACYCLIC_LOCAL_OWNER_DIR",
            directory
                .parent()
                .expect("workspace parent missing")
                .join("owner"),
        )
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    command
}

pub async fn hook(directory: &Path, host: &str, event: &str, input: Value) -> Result<Value> {
    let mut child = command(directory).args(["__hook", host, event]).spawn()?;
    child
        .stdin
        .take()
        .ok_or("hook stdin missing")?
        .write_all(&serde_json::to_vec(&input)?)
        .await?;
    let output = child.wait_with_output().await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

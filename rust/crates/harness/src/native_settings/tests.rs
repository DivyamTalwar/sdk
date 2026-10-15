use super::*;
use crate::mcp::McpTransport;
use serde_json::json;

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    codex: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir().map_err(Error::process_io)?;
        let home = root.path().join("home");
        let workspace = root.path().join("project");
        let codex = root.path().join("codex");
        for path in [&home, &workspace, &codex] {
            std::fs::create_dir(path).map_err(Error::process_io)?;
        }
        Ok(Self {
            _root: root,
            home,
            workspace,
            codex,
        })
    }

    fn config(&self) -> SettingsConfig {
        SettingsConfig::new(&self.workspace, &self.home).codex_home(&self.codex)
    }
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| Error::Invalid("missing parent".into()))?,
    )
    .map_err(Error::process_io)?;
    std::fs::write(path, bytes).map_err(Error::process_io)
}

fn executable<'a>(config: &'a McpConfig, name: &str) -> Option<&'a str> {
    config.servers().find_map(|(key, server)| {
        if key != name {
            return None;
        }
        match &server.transport {
            McpTransport::Stdio { command, .. } => Some(command.as_str()),
            McpTransport::Http { .. } => None,
        }
    })
}

#[tokio::test]
async fn user_project_and_local_scopes_have_whole_entry_precedence() -> Result<()> {
    let fixture = Fixture::new()?;
    let canonical = std::fs::canonicalize(&fixture.workspace).map_err(Error::process_io)?;
    let user = json!({"mcpServers": {
        "user":{"command":"user"}, "shared":{"command":"claude", "args":["discard"]},
        "local":{"command":"user"}, "disabled":{"command":"user"}},
        "projects": {canonical.to_string_lossy().as_ref(): {"mcpServers":{"local":{"command":"local"}}}}});
    write(
        &fixture.home.join(".claude.json"),
        serde_json::to_vec(&user).map_err(|error| Error::Invalid(error.to_string()))?,
    )?;
    write(
        &fixture.codex.join("config.toml"),
        "[mcp_servers.shared]\ncommand='codex'\n[mcp_servers.disabled]\nenabled=false\n",
    )?;
    write(
        &fixture.workspace.join(".mcp.json"),
        br#"{"mcpServers":{"project":{"command":"project"},"local":{"command":"project"}}}"#,
    )?;
    write(
        &fixture.workspace.join(".codex/config.toml"),
        "[mcp_servers.shared]\ncommand='project-codex'\n",
    )?;
    let defaults = NativeSettings::load(fixture.config()).await?;
    assert_eq!(
        defaults.mcp.server_names().collect::<Vec<_>>(),
        ["local", "shared", "user"]
    );
    assert_eq!(executable(&defaults.mcp, "shared"), Some("codex"));
    assert_eq!(executable(&defaults.mcp, "local"), Some("local"));
    let trusted = NativeSettings::load(fixture.config().trust_project_mcp(true)).await?;
    assert_eq!(
        trusted.mcp.server_names().collect::<Vec<_>>(),
        ["local", "project", "shared", "user"]
    );
    assert_eq!(executable(&trusted.mcp, "shared"), Some("project-codex"));
    assert_eq!(executable(&trusted.mcp, "local"), Some("local"));
    let (_, shared) = trusted
        .mcp
        .servers()
        .find(|(name, _)| *name == "shared")
        .ok_or_else(|| Error::Invalid("missing shared server".into()))?;
    assert!(matches!(&shared.transport, McpTransport::Stdio { args, .. } if args.is_empty()));
    Ok(())
}

#[tokio::test]
async fn untrusted_project_files_are_not_read() -> Result<()> {
    let fixture = Fixture::new()?;
    for name in [
        ".mcp.json",
        ".codex/config.toml",
        ".codex/hooks.json",
        ".claude/settings.json",
        ".claude/settings.local.json",
    ] {
        write(&fixture.workspace.join(name), b"invalid configuration")?;
    }
    let snapshot = NativeSettings::load(fixture.config()).await?;
    assert_eq!(snapshot.mcp.server_names().count(), 0);
    assert!(
        NativeSettings::load(fixture.config().trust_project_mcp(true))
            .await
            .is_err()
    );
    #[cfg(feature = "hooks")]
    assert!(
        NativeSettings::load(fixture.config().trust_project_hooks(true))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn local_project_selection_accepts_the_configured_workspace_spelling() -> Result<()> {
    let fixture = Fixture::new()?;
    let configured = fixture.workspace.join("..").join("project");
    let document = json!({"projects": {configured.to_string_lossy().as_ref(): {
        "mcpServers":{"local":{"command":"local"}}
    }}});
    write(
        &fixture.home.join(".claude.json"),
        serde_json::to_vec(&document).map_err(|error| Error::Invalid(error.to_string()))?,
    )?;
    let snapshot = NativeSettings::load(
        SettingsConfig::new(&configured, &fixture.home).codex_home(&fixture.codex),
    )
    .await?;
    assert_eq!(executable(&snapshot.mcp, "local"), Some("local"));
    Ok(())
}

#[cfg(feature = "hooks")]
#[tokio::test]
async fn project_hook_and_server_trust_are_independent() -> Result<()> {
    let fixture = Fixture::new()?;
    let path = fixture.workspace.join(".codex/config.toml");
    write(
        &path,
        "[mcp_servers.project]\ncommand='server'\n[[hooks.PreToolUse]]\nmatcher='['\n[[hooks.PreToolUse.hooks]]\ntype='command'\ncommand='guard'\n",
    )?;
    let snapshot = NativeSettings::load(fixture.config().trust_project_mcp(true)).await?;
    assert_eq!(snapshot.mcp.server_names().collect::<Vec<_>>(), ["project"]);
    assert!(
        NativeSettings::load(fixture.config().trust_project_hooks(true))
            .await
            .is_err()
    );
    write(
        &path,
        "[mcp_servers.project]\ncommand=42\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype='command'\ncommand='guard'\n",
    )?;
    let snapshot = NativeSettings::load(fixture.config().trust_project_hooks(true)).await?;
    assert_eq!(snapshot.mcp.server_names().count(), 0);
    assert!(
        NativeSettings::load(fixture.config().trust_project_mcp(true))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn settings_reads_have_positive_finite_byte_bounds() -> Result<()> {
    let fixture = Fixture::new()?;
    write(&fixture.codex.join("config.toml"), b"[mcp_servers]\n")?;
    assert!(
        NativeSettings::load(fixture.config().maximum_file_bytes(14))
            .await
            .is_ok()
    );
    for maximum in [0, 13, u64::MAX] {
        assert!(
            NativeSettings::load(fixture.config().maximum_file_bytes(maximum))
                .await
                .is_err()
        );
    }
    write(&fixture.codex.join("config.toml"), [0xff])?;
    assert!(NativeSettings::load(fixture.config()).await.is_err());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_deduplication_combines_hook_and_mcp_roles() -> Result<()> {
    let fixture = Fixture::new()?;
    let source = fixture.codex.join("hooks.json");
    write(&source, br#"{"mcpServers":{"shared":{"command":"server"}},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"guard"}]}]}}"#)?;
    std::os::unix::fs::symlink(&source, fixture.workspace.join(".mcp.json"))
        .map_err(Error::process_io)?;
    let snapshot = NativeSettings::load(fixture.config().trust_project_mcp(true)).await?;
    assert_eq!(snapshot.mcp.server_names().collect::<Vec<_>>(), ["shared"]);
    Ok(())
}

#[test]
fn codex_home_uses_the_configured_environment_path() {
    let home = Path::new("user");
    assert_eq!(codex_directory(home, None), home.join(".codex"));
    assert_eq!(
        codex_directory(home, Some(std::ffi::OsStr::new(""))),
        home.join(".codex")
    );
    assert_eq!(
        codex_directory(home, Some(std::ffi::OsStr::new("relocated"))),
        PathBuf::from("relocated")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_aliases_keep_the_highest_declared_scope() -> Result<()> {
    let fixture = Fixture::new()?;
    let source = fixture.home.join(".claude.json");
    write(
        &source,
        br#"{"mcpServers":{"shared":{"command":"claude"}}}"#,
    )?;
    write(
        &fixture.codex.join("config.toml"),
        "[mcp_servers.shared]\ncommand='codex'\n",
    )?;
    std::os::unix::fs::symlink(&source, fixture.workspace.join(".mcp.json"))
        .map_err(Error::process_io)?;
    let user = NativeSettings::load(fixture.config()).await?;
    assert_eq!(executable(&user.mcp, "shared"), Some("codex"));
    let project = NativeSettings::load(fixture.config().trust_project_mcp(true)).await?;
    assert_eq!(executable(&project.mcp, "shared"), Some("claude"));
    Ok(())
}

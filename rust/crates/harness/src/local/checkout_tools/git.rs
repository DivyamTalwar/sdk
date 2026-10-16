//! The shared git facade over the active checkout view.
use super::*;

impl CheckoutTools {
    pub(super) async fn fs(
        &self,
        state: &mut State,
        command: &str,
        context: &ToolContext,
    ) -> Result<ToolResult> {
        use acyclic_fs::access::Action;
        let parsed = crate::tools::parse_fs_command(command)?;
        let args = &parsed.arguments;
        let (relative, mount) = state.mounts.named(parsed.volume.as_deref())?;
        let read_only = matches!(args[0].as_str(), "diff" | "log" | "show" | "status");
        if !relative.is_empty() && !read_only && !matches!(args[0].as_str(), "merge" | "discard") {
            return Err(Error::Unsupported(
                "--volume supports diff, log, show, status, merge and discard".into(),
            ));
        }
        let now = self.clock.now_unix_millis() / 1000;
        for (action, operation) in std::iter::once((Action::Read, VolumeOperation::Read))
            .chain((!read_only).then_some((Action::Write, VolumeOperation::Write)))
        {
            mount.check(action, "", now)?;
            let grant = mount.view.volume.capability(operation, &mut None)?;
            if !context.effective_scope().grants().contains(&grant) {
                return Err(Error::Unauthorized(
                    "fs target volume is not granted".into(),
                ));
            }
        }
        let source = mount
            .view
            .source
            .as_ref()
            .ok_or_else(|| Error::NotFound("volume view is not bound".into()))?;
        let primary = relative.is_empty();
        let selected = source.workspace.view();
        let output = crate::tools::git::run(
            crate::tools::git::Workspace {
                filesystem: &self.workspace.filesystem,
                store: &self.workspace.store,
                view: selected,
                root: source
                    .native
                    .as_ref()
                    .map_or(self.workspace.path.as_path(), |native| native.path()),
                repository: if primary {
                    self.workspace.repository
                } else {
                    source.workspace.workspace().id()
                },
                lease: None,
                checkout: source.native.is_none().then_some(self.workspace.identity),
                author: "LocalHarness",
                authored_at_seconds: i64::try_from(self.clock.now_unix_millis() / 1000)
                    .unwrap_or(i64::MAX),
            },
            args,
        )
        .await;
        // A merge may publish conflict markers before returning an error.
        // Refresh its actual mounted target before exposing that error.
        if let Some(native) = &source.native {
            native.advance_to_head().await?;
        }
        let output = output?;
        let command = output.command.ok_or_else(|| {
            Error::Conflict("recovered a pending fs transition; retry the command".into())
        })?;
        if let Some(switched) = output.switched {
            let switched = switched.into_checkout()?;
            let (_, mount) = state
                .mounts
                .mounts_mut()
                .find(|(path, _)| path.is_empty())
                .ok_or_else(|| Error::NotFound("primary mount is not bound".into()))?;
            mount.view.source = Some(Arc::new(workspace::MountedVolume {
                workspace: crate::tools::git::OwnedView::Checkout(switched),
                native: None,
            }));
        }
        Ok(ToolResult {
            value: serde_json::json!({"result":command}),
        })
    }
}

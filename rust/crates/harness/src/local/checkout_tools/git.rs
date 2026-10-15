//! The shared git facade over the active checkout view.
use super::*;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

impl CheckoutTools {
    pub(super) async fn fs(
        &self,
        view: &workspace::View,
        command: &str,
    ) -> Result<(ToolResult, Option<workspace::View>)> {
        use crate::tools::git::*;
        let mut argv = crate::tools::parse_command(command)?;
        if argv.first().is_some_and(|arg| arg == "git") {
            argv.remove(0);
        }
        let repository_id = self.workspace.view().workspace().id();
        let tree = git_workspace_tree(view, &argv, None)
            .await
            .map_err(storage)?;
        let patch = git_apply_patch(view, &self.workspace.path, &argv)
            .await
            .map_err(storage)?;
        let (ignore, merge_drivers) = git_policy(view).await.map_err(storage)?;
        let executor = RootMaterializingGitExecutor {
            inner: WorkspaceGitExecutor {
                fs: &self.workspace.filesystem,
                store: &self.workspace.store,
                current: view.workspace().clone(),
                lazy_current: view,
                repository_id,
                ignore,
                permit: PublicationPermit::Unrestricted,
                lease: None,
                merge_drivers,
                merge_cache: AsyncMutex::new(Default::default()),
                switched: Mutex::new(None),
            },
            root: &self.workspace.path,
            source_root_identity: self.workspace.identity,
        };
        let repository =
            acyclic_fs::GitCompatRepository::new(repository_id, self.workspace.store.clone());
        let output = run_git_command(
            &repository,
            &argv,
            tree,
            patch.as_deref(),
            "LocalHarness",
            i64::try_from(self.clock.now_unix_millis() / 1000).unwrap_or(i64::MAX),
            &executor,
        )
        .await
        .map_err(storage)?
        .ok_or_else(|| {
            Error::Conflict("recovered a pending fs transition; retry the command".into())
        })?;
        let switched = executor.inner.switched_workspace().map_err(storage)?;
        Ok((
            ToolResult {
                value: serde_json::json!({"result": output}),
            },
            switched,
        ))
    }
}

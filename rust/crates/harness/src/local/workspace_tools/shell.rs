use super::*;
use acyclic_fs::{CancellationToken, MaterializeOptions, NativeMaterializationWork, WorkBudget};
use std::path::PathBuf;

pub(super) async fn run(
    workspace: &LocalWorkspace,
    command: &str,
    context: &ToolContext,
) -> Result<serde_json::Value> {
    let cancellation = CancellationToken::new();
    let work = NativeMaterializationWork::new(WorkBudget::UNBOUNDED, &cancellation)
        .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
    let root = workspace.directory.join("commands");
    let destination = root.join(context.operation_id().to_string());
    std::fs::create_dir_all(&destination).map_err(storage)?;
    let options = MaterializeOptions {
        destination,
        expected_root_identity: None,
        maximum_directory_entries: 4096,
        maximum_extent_spans: 65_536,
        transfer_bytes: 64 * 1024,
    };
    let generation = Box::pin(workspace.view().exactify(
        "/",
        PublicationPermit::Unrestricted,
        &mut acyclic_fs::kernel::KernelAccounting::NativeOriginal {
            work: &work,
            cancellation: &cancellation,
        },
    ))
    .await
    .map_err(storage)?;
    let mount = acyclic_fs::MountOptions::read_write()
        .publication(acyclic_fs::MountPublication::Manual)
        .native_context(&work, None, None);
    let view = Box::pin(generation.prepare_native_working_set(
        &mount,
        &options,
        WorkBudget::UNBOUNDED,
        &cancellation,
    ))
    .await
    .map_err(storage)?;
    let result = capture(view.path().to_path_buf(), command, context).await;
    let owner = context
        .task()
        .owned_mailbox
        .clone()
        .ok_or_else(|| Error::Unauthorized("shell has no execution owner".into()))?;
    let publication =
        Box::pin(
            view.sync_with_permit(PublicationPermit::Unrestricted, &work, move |_| {
                let owner = owner.clone();
                async move { owner.verify_owner(None)?.await }
            }),
        )
        .await
        .map_err(
            |failure: acyclic_fs::native_mount::NativeMountPublicationFailure<Error>| failure.cause,
        );
    view.retire(&cancellation).map_err(storage)?;
    crate::filesystem::native_paths::remove_tree_checked(&root, view.path()).map_err(storage)?;
    publication?;
    result
}

async fn capture(root: PathBuf, command: &str, context: &ToolContext) -> Result<serde_json::Value> {
    let text = command.to_owned();
    let maximum = usize::try_from(context.effective_scope().limits().file_bytes)
        .map_err(|_| Error::Invalid("shell output limit exceeds usize".into()))?;
    let owner = context
        .task()
        .owned_mailbox
        .clone()
        .ok_or_else(|| Error::Unauthorized("shell has no execution owner".into()))?;
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(text)
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut process =
            acyclic_native_runtime::ProcessTree::spawn(&mut command).map_err(storage)?;
        let output = process
            .wait_with_output(
                std::time::Duration::from_secs(60),
                maximum,
                None,
                || {
                    runtime
                        .block_on(async { owner.verify_owner(None)?.await })
                        .map_err(|error| std::io::Error::other(error.to_string()))
                },
                |_| Ok(Default::default()),
                None,
                None,
            )
            .map_err(|failure| storage(failure.error))?;
        Ok(serde_json::json!({
            "success": output.status.success(), "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))
    })
    .await
    .map_err(storage)?
}

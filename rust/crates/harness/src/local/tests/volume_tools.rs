//! Configured volumes use the same persistent view and stock coding tools.
use super::*;
use acyclic_fs::{CancellationToken, WorkBudget};

#[tokio::test]
async fn configured_volume_binds_bash_and_fs_and_reopens_the_selected_branch() -> Result<()> {
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    let project = volume(directory.path()).await?;
    let provider = scripted([
        ScriptedTurn::Events(vec![
            ScriptedEvent::Event(ModelEvent::ToolCall {
                call_id: "write".into(),
                name: "write".into(),
                arguments: json!({"path":"note.txt","text":"volume","media_type":"text/plain","display_name":"note.txt"}),
            }),
            ScriptedEvent::Event(ModelEvent::Completed {
                metadata: json!({}),
            }),
        ]),
        super::checked_commands::call(
            1,
            "bash",
            "test $(cat note.txt) = volume; printf shell > shell.txt",
        ),
        super::checked_commands::call(2, "fs", "commit -m volume"),
        super::checked_commands::call(3, "fs", "checkout -b agents/child"),
        super::checked_commands::call(4, "bash", "printf child > note.txt"),
        ScriptedTurn::Text("done".into()),
        super::checked_commands::call(5, "fs", "status"),
        super::checked_commands::call(6, "bash", "test $(cat note.txt) = child"),
        super::checked_commands::call(7, "fs", "checkout main"),
        super::checked_commands::call(
            8,
            "bash",
            "test $(cat note.txt) = volume; test $(cat shell.txt) = shell",
        ),
        ScriptedTurn::Text("done".into()),
    ]);
    let harness = LocalHarness::open(volume_config(
        directory.path(),
        project.clone(),
        provider.clone(),
    )?)
    .await?;
    super::checkout_reopen::run(&harness).await?;
    let checkout = harness.workspace().unwrap();
    let head = checkout
        .view()
        .workspace()
        .head(WorkBudget::UNBOUNDED, &CancellationToken::new())
        .await
        .map_err(super::super::storage)?
        .value;
    assert_eq!(
        head.read(
            "/note.txt",
            64,
            WorkBudget::UNBOUNDED,
            &CancellationToken::new()
        )
        .await
        .map_err(super::super::storage)?
        .value
        .as_ref(),
        b"volume"
    );
    assert_eq!(
        head.read(
            "/shell.txt",
            64,
            WorkBudget::UNBOUNDED,
            &CancellationToken::new()
        )
        .await
        .map_err(super::super::storage)?
        .value
        .as_ref(),
        b"shell"
    );
    let view = checkout.path.clone();
    drop(head);
    drop(harness);
    let harness = LocalHarness::open(volume_config(directory.path(), project, provider)?).await?;
    assert_eq!(harness.workspace().unwrap().path, view);
    super::checkout_reopen::run(&harness).await?;
    assert_eq!(
        std::fs::read(view.join("note.txt")).map_err(super::super::storage)?,
        b"volume"
    );
    Ok(())
}

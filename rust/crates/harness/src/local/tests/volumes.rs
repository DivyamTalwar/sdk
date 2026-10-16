//! Real native composition uses each granted volume's publication owner.
use super::*;

#[tokio::test]
async fn fs_volume_qualifier_selects_only_a_granted_mount() -> Result<()> {
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    let provider = scripted([
        super::checkout::call(
            0,
            "bash",
            "printf root > root-file; printf data > .volumes/scratch/data-file",
        ),
        super::checkout::call(1, "fs", "status --volume scratch"),
        super::checkout::call(2, "fs", "git status --volume project"),
        super::checkout::call(3, "fs", "diff --volume scratch"),
        super::checkout::call(4, "fs", "status --volume journal"),
        super::checkout::call(5, "fs", "status --volume ../scratch"),
        ScriptedTurn::Text("inspected".into()),
    ]);
    let harness = LocalHarness::open(LocalHarnessConfig::new(
        directory.path(),
        (model()?, provider.clone()),
    ))
    .await?;
    assert_eq!(
        harness.run("inspect the bound volumes").await?.text,
        "inspected"
    );
    let workspace = harness.workspace().unwrap();
    let scratch = workspace
        .mounts
        .named(Some("scratch"))?
        .1
        .view
        .source
        .as_ref()
        .unwrap();
    let requests = provider.requests();
    let results: std::collections::BTreeMap<_, _> = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|message| message.content.parts())
        .filter_map(|part| match part {
            crate::model::ModelContentPart::ToolResult {
                call_id,
                content: crate::model::ToolResultContent::Json { value },
                ..
            } => Some((call_id.as_str(), value)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 6);
    for (call, expected) in [
        ("checkout-1", scratch.workspace.workspace().id()),
        ("checkout-2", workspace.view().workspace().id()),
    ] {
        let output: acyclic_fs::GitCommandOutput =
            serde_json::from_value(results[call]["result"].clone())
                .map_err(super::super::storage)?;
        let acyclic_fs::GitCommandOutput::Status(status) = output else {
            panic!("expected status")
        };
        assert_eq!(status.workspace.workspace_id(), expected);
    }
    let diff: acyclic_fs::GitCommandOutput =
        serde_json::from_value(results["checkout-3"]["result"].clone())
            .map_err(super::super::storage)?;
    assert!(matches!(diff, acyclic_fs::GitCommandOutput::Filesystem(_)));
    for call in ["checkout-4", "checkout-5"] {
        assert!(
            results[call].to_string().contains("volume is not mounted"),
            "{}",
            results[call]
        );
    }
    assert_eq!(
        std::fs::read(directory.path().join("root-file")).map_err(super::super::storage)?,
        b"root"
    );
    assert_eq!(
        std::fs::read(scratch.native.path().join("data-file")).map_err(super::super::storage)?,
        b"data"
    );
    Ok(())
}

#[tokio::test]
async fn reopened_checkout_recovers_its_selected_branch_from_the_git_catalog() -> Result<()> {
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    let first = scripted([
        super::checkout::call(0, "bash", "printf base > note"),
        super::checkout::call(1, "fs", "add note"),
        super::checkout::call(2, "fs", "commit -m base"),
        super::checkout::call(3, "fs", "checkout -b topic"),
        super::checkout::call(4, "bash", "printf child > note"),
        ScriptedTurn::Text("branched".into()),
    ]);
    let repository;
    {
        let harness =
            LocalHarness::open(LocalHarnessConfig::new(directory.path(), (model()?, first)))
                .await?;
        assert_eq!(harness.run("make a branch").await?.text, "branched");
        let workspace = harness.workspace().unwrap();
        repository = workspace.repository;
    }
    let second = scripted([
        super::checkout::call(
            0,
            "bash",
            "test \"$(cat note)\" = child && printf resumed > after",
        ),
        super::checkout::call(1, "fs", "status"),
        ScriptedTurn::Text("resumed".into()),
    ]);
    let harness = LocalHarness::open(LocalHarnessConfig::new(
        directory.path(),
        (model()?, second.clone()),
    ))
    .await?;
    let workspace = harness.workspace().unwrap();
    assert_eq!(workspace.repository, repository);
    assert_ne!(workspace.view().workspace().id(), repository);
    assert_eq!(
        harness.run("continue the selected branch").await?.text,
        "resumed"
    );
    let requests = second.requests();
    let statuses: Vec<_> = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|message| message.content.parts())
        .filter_map(|part| match part {
            crate::model::ModelContentPart::ToolResult { name, content, .. } if name == "fs" => {
                Some(content)
            }
            _ => None,
        })
        .collect();
    assert_eq!(statuses.len(), 1);
    let status = serde_json::to_string(statuses[0]).map_err(super::super::storage)?;
    assert!(!status.contains("\"error\""), "{status}");
    assert!(status.contains("topic"), "{status}");
    let branch = workspace
        .view()
        .workspace()
        .head(
            acyclic_fs::WorkBudget::UNBOUNDED,
            &acyclic_fs::CancellationToken::new(),
        )
        .await
        .map_err(super::super::storage)?
        .value;
    assert_eq!(
        branch
            .read(
                "/after",
                32,
                acyclic_fs::WorkBudget::UNBOUNDED,
                &acyclic_fs::CancellationToken::new()
            )
            .await
            .map_err(super::super::storage)?
            .value
            .as_ref(),
        b"resumed"
    );
    Ok(())
}

#[tokio::test]
async fn bash_crosses_granted_volumes_without_capturing_a_mount_in_the_project() -> Result<()> {
    let directory = tempfile::tempdir().map_err(super::super::storage)?;
    std::fs::create_dir(directory.path().join(".volumes")).map_err(super::super::storage)?;
    std::fs::write(directory.path().join(".volumes/user-owned"), b"retain")
        .map_err(super::super::storage)?;
    let provider = scripted([
        ScriptedTurn::Events(vec![
            ScriptedEvent::Event(ModelEvent::ToolCall {
                call_id: "seed".into(),
                name: "write".into(),
                arguments: json!({"path":"seed", "text":"from tool\n", "media_type":"text/plain", "display_name":"seed"}),
            }),
            ScriptedEvent::Event(ModelEvent::Completed {
                metadata: json!({}),
            }),
        ]),
        super::checkout::call(
            1,
            "bash",
            r#"test "$ACYCLIC_VOLUME_SCRATCH_MODE" = rw && IFS= read -r text < .volumes/scratch/seed && printf '%s' "$text" > copied && printf scratch > .volumes/scratch/created && test ! -e .volumes/journal"#,
        ),
        super::checkout::call(2, "fs", "status"),
        ScriptedTurn::Text("composed".into()),
    ]);
    let harness = LocalHarness::open(LocalHarnessConfig::new(
        directory.path(),
        (model()?, provider.clone()),
    ))
    .await?;
    assert_eq!(
        harness.run("copy across the granted volumes").await?.text,
        "composed"
    );
    assert_eq!(
        std::fs::read(directory.path().join("copied")).map_err(super::super::storage)?,
        b"from tool"
    );
    let entries: Vec<_> = std::fs::read_dir(directory.path().join(".volumes"))
        .map_err(super::super::storage)?
        .filter_map(|entry| match entry {
            Ok(entry) => match entry.file_type() {
                Ok(kind) if kind.is_dir() => Some(Ok(entry.file_name())),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            },
            Err(error) => Some(Err(error)),
        })
        .collect::<std::io::Result<_>>()
        .map_err(super::super::storage)?;
    assert_eq!(entries, [std::ffi::OsString::from("scratch")]);
    let workspace = harness.workspace().unwrap();
    let cancellation = acyclic_fs::CancellationToken::new();
    let budget = acyclic_fs::WorkBudget::UNBOUNDED;
    let primary = workspace
        .view()
        .workspace()
        .head(budget, &cancellation)
        .await
        .map_err(super::super::storage)?
        .value;
    assert_eq!(
        primary
            .read("/.volumes/user-owned", 32, budget, &cancellation)
            .await
            .map_err(super::super::storage)?
            .value
            .as_ref(),
        b"retain"
    );
    for path in acyclic_fs::native_mount_control_paths(&directory.path().join(".volumes/scratch"))
        .map_err(Error::from)?
    {
        let relative = path
            .strip_prefix(directory.path())
            .map_err(super::super::storage)?;
        let relative = format!("/{}", relative.to_str().unwrap().replace('\\', "/"));
        assert!(matches!(
            primary.stat(&relative, budget, &cancellation).await,
            Err(acyclic_fs::OperationFailure {
                error: acyclic_fs::WorkspaceError::NotFound,
                ..
            })
        ));
    }
    assert_eq!(
        primary
            .read("/copied", 32, budget, &cancellation)
            .await
            .map_err(super::super::storage)?
            .value
            .as_ref(),
        b"from tool"
    );
    assert!(matches!(
        primary
            .stat("/.volumes/scratch/created", budget, &cancellation)
            .await,
        Err(acyclic_fs::OperationFailure {
            error: acyclic_fs::WorkspaceError::NotFound,
            ..
        })
    ));
    let scratch = workspace
        .filesystem
        .open_workspace(
            &harness.scratch().storage_name(&mut None)?,
            budget,
            &cancellation,
        )
        .await
        .map_err(super::super::storage)?
        .value;
    let scratch = scratch
        .head(budget, &cancellation)
        .await
        .map_err(super::super::storage)?
        .value;
    assert_eq!(
        scratch
            .read("/created", 32, budget, &cancellation)
            .await
            .map_err(super::super::storage)?
            .value
            .as_ref(),
        b"scratch"
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    let results: Vec<_> = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|message| message.content.parts())
        .filter_map(|part| match part {
            crate::model::ModelContentPart::ToolResult { name, content, .. } => {
                Some((name, content))
            }
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 3);
    for (name, content) in results {
        let result = serde_json::to_string(content).map_err(super::super::storage)?;
        assert!(!result.contains("\"error\""), "{name}: {result}");
        if name == "fs" {
            assert!(!result.contains("created"), "{result}");
            assert!(!result.contains("acyclic-fs-mount"), "{result}");
        }
    }
    Ok(())
}

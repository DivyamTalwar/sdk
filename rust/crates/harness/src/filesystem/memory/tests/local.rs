use super::*;

#[tokio::test]
async fn local_builder_runs_through_ref_only_storage() -> Result<()> {
    let storage = MemoryHarnessStorage::new(AgentId::from_bytes([81; 16]), 4_096).await?;
    let provider = Arc::new(text_model(2));
    let bundle = storage
        .builder()
        .without_default_tools()
        .compaction(crate::context::CompactionPolicy::Disabled)
        .model(
            Model::new("test".into(), "text".into(), "1".into(), Value::Null)?,
            provider.clone(),
        )
        .grant("model:generate")
        .build()?;
    let operation_id = OperationId::from_bytes([82; 16]);
    let attachment = storage
        .stage(
            operation_id,
            "input/note.txt",
            b"private attachment body",
            "text/plain",
            "note.txt",
        )
        .await?;
    let user = storage
        .stage(
            operation_id,
            "input/prompt.txt",
            b"hello",
            "text/plain",
            "prompt.txt",
        )
        .await?;
    assert_eq!(
        storage.read(&attachment).await?.as_slice(),
        b"private attachment body"
    );
    let reader = AgentId::from_bytes([83; 16]);
    let delegated = storage
        .delegate_file_read(reader, "attached-reader", &attachment)
        .await?;
    assert_eq!(delegated.agent(), Some(reader));
    assert_eq!(
        storage
            .read_delegated(&delegated, &attachment)
            .await?
            .as_slice(),
        b"private attachment body"
    );
    let root_directory = storage.delegate_directory_read(reader, "attached-root", "")?;
    assert_eq!(
        storage
            .read_delegated(&root_directory, &attachment)
            .await?
            .as_slice(),
        b"private attachment body"
    );
    assert!(
        ContentGrant::verify(
            &storage.verifier(),
            &delegated,
            storage.volume(),
            VolumeOperation::Write,
            u64::MAX,
            &mut None
        )
        .is_err()
    );
    let directory = storage.delegate_directory_read(reader, "attached-directory", "input")?;
    let (generation, listed) = storage
        .list_delegated(&directory, "input", "input", None, None, 16)
        .await?;
    assert!(listed.entries.iter().any(|entry| {
        entry
            .name
            .unicode_text(
                acyclic_fs::WorkBudget::UNBOUNDED,
                &acyclic_fs::CancellationToken::new(),
            )
            .ok()
            .map(|receipt| receipt.value)
            .as_deref()
            == Some("note.txt")
    }));
    let (from_directory, body) = storage
        .read_delegated_path(&directory, "input", "input/note.txt", Some(&generation))
        .await?;
    assert_eq!(body.as_ref(), b"private attachment body");
    assert_eq!(from_directory.path(), attachment.path());
    assert!(
        storage
            .read_delegated_path(&directory, "input", "inputs/note.txt", Some(&generation))
            .await
            .is_err()
    );
    storage
        .stage(
            OperationId::from_bytes([84; 16]),
            "input/new.txt",
            b"new",
            "text/plain",
            "new.txt",
        )
        .await?;
    assert!(
        storage
            .read_delegated_path(&directory, "input", "input/note.txt", Some(&generation))
            .await
            .is_ok()
    );
    assert!(
        ContentGrant::verify(
            &storage.verifier(),
            &directory,
            storage.volume(),
            VolumeOperation::Write,
            u64::MAX,
            &mut None
        )
        .is_err()
    );
    let output = storage
        .run_conversation(
            &bundle,
            operation_id,
            user.clone(),
            vec![Attachment {
                file: attachment.clone(),
                label: None,
            }],
            2,
        )
        .await?;
    assert_eq!(output.text, "local response");
    let first_aggregate = storage
        .journal
        .conversation_projection(bundle.limits())
        .await?;
    let first_user = first_aggregate
        .reducer()
        .conversation()
        .and_then(|conversation| conversation.messages.first())
        .ok_or_else(|| Error::Invalid("conversation user message is missing".into()))?;
    assert_eq!(
        first_user.id,
        crate::turn::derived_uuid(operation_id, "user")
    );
    drop(first_aggregate);
    let replayed = storage
        .run_conversation(
            &bundle,
            operation_id,
            user.clone(),
            vec![Attachment {
                file: attachment.clone(),
                label: None,
            }],
            2,
        )
        .await?;
    assert_eq!(replayed, output);
    let mut aggregate = storage
        .journal
        .conversation_projection(bundle.limits())
        .await?;
    assert_eq!(
        aggregate
            .reducer()
            .conversation()
            .ok_or_else(|| crate::Error::Invalid("conversation not bound".into()))?
            .messages
            .len(),
        2
    );
    assert_eq!(aggregate.reducer().resident_context_selections().count(), 1);
    let expected_selection = aggregate
        .context_selection_for_operation(operation_id)
        .await?
        .ok_or_else(|| Error::Invalid("original context selection missing".into()))?;
    aggregate.set_resident_event_limit(1)?;
    assert_eq!(aggregate.reducer().resident_context_selections().count(), 0);
    assert!(
        aggregate
            .reducer()
            .context_selection_for_operation(operation_id)
            .is_none()
    );
    assert_eq!(
        aggregate
            .context_selection_for_operation(operation_id)
            .await?,
        Some(expected_selection)
    );
    drop(aggregate);
    storage.run_prompt(&bundle, "follow-up", Vec::new()).await?;
    assert_eq!(
        storage
            .run_conversation(
                &bundle,
                operation_id,
                user,
                vec![Attachment {
                    file: attachment,
                    label: None
                }],
                2
            )
            .await?,
        output
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.tools.is_empty()));
    assert_eq!(requests[0].messages.len(), 1);
    assert_eq!(requests[1].messages.len(), 3);
    Ok(())
}

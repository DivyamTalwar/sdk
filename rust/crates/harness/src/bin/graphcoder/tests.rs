//! Executable composition uses the real shared ClientViews and Markdown renderer.
use acyclic_harness::{
    OperationId,
    client::ClientViews,
    model::ModelEvent,
    tui::{
        ClientRenderer, PromptInput, Viewport,
        ratatui::{Terminal, backend::TestBackend},
    },
    wire,
};

fn picture(renderer: &ClientRenderer) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            renderer.screen().render(
                frame,
                &PromptInput::default(),
                &mut Viewport::default(),
                0,
                1000,
            );
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn shared_markdown_is_visible_and_snapshot_reset_removes_old_text() {
    let owner = wire::Authority {
        kind: wire::AggregateKind::Task as i32,
        id: "task".into(),
    };
    let mut views = ClientViews::default();
    let mut renderer = ClientRenderer::default();
    views
        .model_event(
            &owner,
            OperationId::from_bytes([1; 16]),
            ModelEvent::Content {
                delta: "# Heading\n**streamed answer**".into(),
            },
        )
        .unwrap();
    renderer.update(&views, &owner);
    let visible = picture(&renderer);
    assert!(visible.contains("Heading"));
    assert!(visible.contains("streamed answer"));
    assert!(!visible.contains("**streamed"));
    renderer.update(&ClientViews::default(), &owner);
    assert!(!picture(&renderer).contains("streamed answer"));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    // Property: the composed cells after arbitrary Unicode delta boundaries
    // equal the cells obtained by projecting the same complete ClientViews text.
    #[test]
    fn composed_markdown_is_independent_of_chunking(chunks in proptest::collection::vec(".{0,12}", 0..6)) {
        let owner = wire::Authority { kind: wire::AggregateKind::Task as i32, id: "task".into() };
        let operation = OperationId::from_bytes([1;16]);
        let mut views = ClientViews::default();
        let mut live = ClientRenderer::default();
        live.update(&views, &owner);
        for chunk in &chunks {
            views.model_event(&owner, operation, ModelEvent::Content { delta: chunk.clone() }).unwrap();
            live.update(&views, &owner);
        }
        let mut replayed = ClientRenderer::default();
        replayed.update(&views, &owner);
        proptest::prop_assert_eq!(picture(&live), picture(&replayed));
    }
}

#[test]
fn file_completions_render_in_the_composer_and_enter_only_accepts() {
    use acyclic_harness::tui::{
        Screen,
        key_event::{KeyCode, KeyEvent, KeyModifiers},
    };
    let mut input = PromptInput::default();
    input
        .completion
        .set_file(
            "commands/tools.json",
            Some(br#"[{"name":"read","description":"Read a file","tool":true}]"#),
        )
        .unwrap();
    input.committed_text("/re", 0);
    input.refresh_completions();
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            Screen::default().render(frame, &input, &mut Viewport::default(), 0, 0);
        })
        .unwrap();
    let visible: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(visible.contains("Completions"));
    assert!(visible.contains("/read [tool]"));
    assert!(visible.contains("Read a file"));
    assert!(
        input
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 1, 78)
            .is_empty()
    );
    assert_eq!(input.editor.text(), "/read ");
    input.refresh_completions();
    assert!(!input.completion.active(&input.editor));
    input.committed_text("@rea", 2);
    input
        .completion
        .set_file("workspace/read me.rs", Some(b"source"))
        .unwrap();
    input.refresh_completions();
    assert!(
        input
            .key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 3, 78)
            .is_empty()
    );
    assert_eq!(input.editor.text(), "/read @\"read me.rs\" ");
    input.editor.undo();
    assert_eq!(input.editor.text(), "/read @rea");
}

#[test]
fn followed_documents_and_owner_feedback_render_and_reset() {
    use acyclic_harness::{
        client::{DocumentViews, TaskView},
        core::LifecycleState,
        task_volume::{Component, FileKey, Write},
    };
    let owner = wire::Authority {
        kind: wire::AggregateKind::Task as i32,
        id: "task".into(),
    };
    let key = format!("{}:{}", wire::AggregateKind::Task.as_str_name(), owner.id);
    let mut documents = DocumentViews::default();
    documents
        .apply(&Write::new(FileKey::Goal, b"# Finish rewrite".to_vec()))
        .unwrap();
    documents
        .apply(&Write::new(
            FileKey::Plan(Component::new("layout").unwrap()),
            b"**Compose widgets**".to_vec(),
        ))
        .unwrap();
    let mut views = ClientViews::default();
    views.documents.insert(key.clone(), documents);
    views.tasks.insert(
        key,
        TaskView {
            status: LifecycleState::Active,
            lifecycle_operation_id: None,
            reason: Some("owner feedback".into()),
            parent: None,
        },
    );
    let mut renderer = ClientRenderer::default();
    renderer.update(&views, &owner);
    let visible = picture(&renderer);
    for text in [
        "Finish rewrite",
        "Compose widgets",
        "owner feedback",
        "GOAL.md",
        "plans/",
    ] {
        assert!(visible.contains(text), "missing {text}");
    }
    renderer.update(&ClientViews::default(), &owner);
    let visible = picture(&renderer);
    for text in ["Finish rewrite", "Compose widgets", "owner feedback"] {
        assert!(!visible.contains(text), "stale {text}");
    }
}

#[test]
fn tool_output_deltas_repaint_without_a_new_committed_message() {
    use acyclic_harness::model::ToolCall;
    let owner = wire::Authority {
        kind: wire::AggregateKind::Task as i32,
        id: "task".into(),
    };
    let mut views = ClientViews::default();
    views
        .model_event(
            &owner,
            OperationId::from_bytes([1; 16]),
            ModelEvent::ToolCall(ToolCall::new(
                "call".into(),
                "read".into(),
                Default::default(),
            )),
        )
        .unwrap();
    views.tool_output(&owner, "call", "first chunk");
    let mut renderer = ClientRenderer::default();
    renderer.update(&views, &owner);
    assert!(picture(&renderer).contains("first chunk"));
    views.tool_output(&owner, "call", " second chunk");
    renderer.update(&views, &owner);
    let visible = picture(&renderer);
    assert!(visible.contains("first chunk second chunk"));
    assert!(!visible.contains("Parts {"));
}

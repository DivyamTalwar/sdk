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

fn painted(renderer: &ClientRenderer) -> Terminal<TestBackend> {
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
}

fn picture(renderer: &ClientRenderer) -> String {
    screen_text(&painted(renderer))
}

fn screen_text(terminal: &Terminal<TestBackend>) -> String {
    use acyclic_harness::tui::ratatui::text::Line;
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for row in buffer.content.chunks(usize::from(buffer.area.width).max(1)) {
        let mut column = 0;
        while let Some(cell) = row.get(column) {
            text.push_str(cell.symbol());
            column += Line::raw(cell.symbol()).width().max(1);
        }
        text.push('\n');
    }
    text
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
        let live = painted(&live);
        let replayed = painted(&replayed);
        proptest::prop_assert_eq!(live.backend().buffer(), replayed.backend().buffer());
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
    let visible: String = screen_text(&terminal);
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
    assert!(renderer.is_busy());
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

#[test]
fn follow_tail_keeps_new_output_visible_after_the_screen_fills() {
    use acyclic_harness::tui::{Screen, ratatui::text::Line};
    let rows: Vec<_> = (0..100)
        .map(|n| Line::from(format!("message {n}")))
        .collect();
    let screen = Screen {
        transcript: &rows,
        ..Screen::default()
    };
    let input = PromptInput::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            let scroll = screen.tail_scroll(frame.area(), &input);
            screen.render(frame, &input, &mut Viewport::default(), scroll, 0);
        })
        .unwrap();
    let visible: String = screen_text(&terminal);
    assert!(visible.contains("message 99"));
    assert!(!visible.contains("message 0"));
}

#[test]
fn composed_reverse_search_restores_draft_and_ime_stays_multiline() {
    use acyclic_harness::tui::{
        Editor, PromptHistory, Screen,
        key_event::{KeyCode, KeyEvent, KeyModifiers},
    };
    let mut input = PromptInput::new(Editor::new("draft"));
    let mut history = PromptHistory::default();
    history.push("first 日本語".into());
    history.push("second prompt".into());
    input.replace_history(history);
    input.key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        0,
        78,
    );
    input.committed_text("日本語", 1);
    assert_eq!(input.editor.text(), "first 日本語");
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    let mut viewport = Viewport::default();
    terminal
        .draw(|frame| {
            Screen::default().render(frame, &input, &mut viewport, 0, 1);
        })
        .unwrap();
    let visible: String = screen_text(&terminal);
    assert!(visible.contains("first 日本語"));
    input.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 2, 78);
    assert_eq!(input.editor.text(), "draft");
    input.editor.select_all();
    input.committed_text("日本語\n👩‍💻", 3);
    terminal
        .draw(|frame| {
            Screen::default().render(frame, &input, &mut viewport, 0, 3);
        })
        .unwrap();
    let visible: String = screen_text(&terminal);
    assert!(visible.contains("日本語"));
    assert!(visible.contains("👩‍💻"));
    assert!(!visible.contains("[Pasted"));
    assert!(viewport.cursor.is_some());
}

#[test]
fn composed_message_edit_recording_and_attachment_indicators_survive_layout() {
    use acyclic_harness::tui::{
        Editor, Screen,
        key_event::{KeyCode, KeyEvent, KeyModifiers},
    };
    let mut input = PromptInput::new(Editor::new("live draft"));
    input.images = 2;
    input.recording = true;
    input.edit_message("earlier".into(), "message being edited");
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    let mut viewport = Viewport::default();
    terminal
        .draw(|frame| {
            Screen::default().render(frame, &input, &mut viewport, 0, 0);
        })
        .unwrap();
    let visible: String = screen_text(&terminal);
    assert!(visible.contains("Editing message"));
    assert!(visible.contains("message being edited"));
    input.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 1, 78);
    terminal
        .draw(|frame| {
            Screen::default().render(frame, &input, &mut viewport, 0, 1);
        })
        .unwrap();
    let visible: String = screen_text(&terminal);
    assert!(visible.contains("live draft"));
    assert!(visible.contains("2 images attached"));
    assert!(visible.contains("REC"));
}

#[test]
fn captured_diff_model_renders_text_binary_and_empty_file_changes() {
    use acyclic_harness::{
        client::FileDiff,
        tui::{Screen, diff_lines},
    };
    let mut files = std::collections::BTreeMap::new();
    for (path, base, current) in [
        ("src/main.rs", Some(&b"before\n"[..]), Some(&b"after\n"[..])),
        ("image.bin", Some(&b"\0old"[..]), Some(&b"\0new"[..])),
        ("empty.txt", None, Some(&b""[..])),
    ] {
        files.insert(
            path.to_owned(),
            FileDiff::between(base, current, 4096, 4096)
                .unwrap()
                .unwrap(),
        );
    }
    let rows = diff_lines(&files);
    let screen = Screen {
        diff: &rows,
        ..Screen::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal
        .draw(|frame| {
            screen.render(
                frame,
                &PromptInput::default(),
                &mut Viewport::default(),
                0,
                0,
            );
        })
        .unwrap();
    let visible = screen_text(&terminal);
    for expected in [
        "Diff",
        "src/main.rs (modified) +1 -1",
        "-before",
        "+after",
        "Binary file changed",
        "empty.txt (added)",
    ] {
        assert!(visible.contains(expected), "missing {expected}: {visible}");
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    // Property: arbitrary agent tracking/snapshot order isolates each diff cut;
    // a snapshot for an untracked agent leaves all projections unchanged.
    #[test]
    fn diff_snapshot_cuts_stay_with_their_tracked_agent(actions in proptest::collection::vec((0u8..4, proptest::bool::ANY, proptest::num::u8::ANY), 0..12)) {
        use acyclic_harness::{AgentId, client::{ClientSession, MemoryClientState, DiffView}, conversation::{VolumeRef, VolumeClass, VolumeOwner}, resources::{ProviderRef, GenerationRef}};
        let provider = ProviderRef::new("diff".into(), "filesystem".into(), "2".into(), &mut None).unwrap();
        let volume = VolumeRef::new(provider.clone(), "project".into(), VolumeClass::SessionShared, VolumeOwner::Agent(AgentId::from_bytes([1;16])), &mut None).unwrap();
        let mut session = ClientSession::open(MemoryClientState::default()).unwrap();
        let mut tracked = std::collections::BTreeSet::new();
        let mut expected = std::collections::BTreeMap::new();
        for (agent, track, generation) in actions {
            let authority = wire::Authority { kind: wire::AggregateKind::Task as i32, id: agent.to_string() };
            if track {
                session.track(wire::ReplayCursor { authority: Some(authority.clone()), generation: "history".into(), revision: 0 }).unwrap();
                tracked.insert(agent);
            } else {
                let cut = GenerationRef::new(&provider, &[generation], Some("1"), None).unwrap().0;
                let view = DiffView::new(volume.clone(), cut).unwrap();
                let before = session.views().clone();
                let result = session.replace_diff(&authority, view);
                if tracked.contains(&agent) {
                    proptest::prop_assert!(result.is_ok());
                    expected.insert(format!("{}:{agent}", wire::AggregateKind::Task.as_str_name()), generation);
                } else {
                    proptest::prop_assert!(result.is_err());
                    proptest::prop_assert_eq!(session.views(), &before);
                }
            }
            let actual: std::collections::BTreeMap<_,_> = session.views().diffs.iter().map(|(key, view)| (key.clone(), view.generation().as_resource().key()[0])).collect();
            proptest::prop_assert_eq!(&actual, &expected);
        }
    }
}

#[tokio::test]
async fn completion_popup_follows_real_task_volume_publications() {
    use acyclic_harness::{
        task_volume::{FileKey, Query, TaskVolume, Write},
        tui::{Editor, Screen},
        tui_files::CompletionFiles,
    };
    use acyclic_stream::{MemoryStream, StreamPath};
    let volume = TaskVolume::open(
        std::sync::Arc::new(MemoryStream::default()),
        StreamPath::new("composer").unwrap(),
    )
    .await
    .unwrap();
    let mut input = PromptInput::new(Editor::new("@新"));
    let mut files = CompletionFiles::observe(&volume, &mut input, None, 8)
        .await
        .unwrap();
    assert!(!input.completion.active(&input.editor));
    let cut = volume.snapshot(&Query::Inputs, 8).await.unwrap().generation;
    volume
        .append(
            cut,
            &Write::new(
                FileKey::parse("workspace/新しい.rs").unwrap(),
                b"source".to_vec(),
            ),
        )
        .await
        .unwrap();
    let cursor = files.cursor.clone();
    let change = files.read_change().await.unwrap();
    assert_eq!(files.cursor, cursor);
    assert!(!input.completion.active(&input.editor));
    files.apply_change(change, &mut input).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            Screen::default().render(frame, &input, &mut Viewport::default(), 0, 0);
        })
        .unwrap();
    assert!(screen_text(&terminal).contains("新しい.rs"));
    assert!(input.completion.accept(&mut input.editor, 1));
    assert_eq!(input.editor.text(), "@新しい.rs ");
}

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

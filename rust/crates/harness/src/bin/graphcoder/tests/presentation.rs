use acyclic_harness::{
    OperationId,
    client::{ClientViews, CommandFileRoot, CommandSession},
    model::ModelEvent,
    task_volume::TaskVolume,
    tui::{
        ClientRenderer, PromptInput, Viewport,
        ratatui::{Terminal, backend::TestBackend},
    },
    wire,
};
use acyclic_stream::{MemoryStream, StreamPath};
use std::sync::Arc;

#[tokio::test]
async fn followed_presentation_rows_are_declarative_and_live() {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("presentation.json")).unwrap();
    let volume = Arc::new(
        TaskVolume::open(
            Arc::new(MemoryStream::default()),
            StreamPath::new("presentation").unwrap(),
        )
        .await
        .unwrap(),
    );
    let mut files = CommandSession::open(
        volume,
        OperationId::from_bytes([12; 16])
            .to_string()
            .parse()
            .unwrap(),
        65_536,
        64,
    )
    .await
    .unwrap();
    let owner = wire::Authority {
        kind: wire::AggregateKind::Task as i32,
        id: "task".into(),
    };
    let turn = OperationId::from_bytes([13; 16]);
    let mut views = ClientViews::default();
    views
        .model_event(
            &owner,
            turn,
            ModelEvent::Reasoning {
                delta: "Private reasoning".into(),
            },
        )
        .unwrap();
    views
        .model_event(
            &owner,
            turn,
            ModelEvent::Content {
                delta: "**Visible answer**".into(),
            },
        )
        .unwrap();
    let mut renderer = ClientRenderer::default();
    renderer.update(&views, &owner);
    for row in rows {
        files
            .write_state(
                CommandFileRoot::Config,
                "settings.json",
                &serde_json::json!({"reasoning":row["reasoning"]}),
            )
            .await
            .unwrap();
        let screen = renderer.screen_for(&files).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
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
        let picture = super::screen_text(&terminal);
        for text in row["expected"].as_array().unwrap() {
            assert!(
                picture.contains(text.as_str().unwrap()),
                "{}: {picture}",
                row["row"]
            );
        }
        for text in row["absent"].as_array().unwrap() {
            assert!(
                !picture.contains(text.as_str().unwrap()),
                "{}: {picture}",
                row["row"]
            );
        }
    }
    files
        .write_state(
            CommandFileRoot::Config,
            "settings.json",
            &serde_json::json!({"reasoning":"invalid"}),
        )
        .await
        .unwrap();
    assert!(renderer.screen_for(&files).is_err());
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    // Property: streamed reasoning and live visibility changes paint the same
    // cells as reopening the current ClientViews and settings-file snapshot.
    #[test]
    fn reasoning_visibility_and_chunking_are_replay_equivalent(chunks in proptest::collection::vec((".{0,12}", proptest::bool::ANY), 1..6)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let volume = Arc::new(TaskVolume::open(Arc::new(MemoryStream::default()), StreamPath::new("reasoning-replay").unwrap()).await.unwrap());
            let mut files = CommandSession::open(volume, OperationId::from_bytes([12;16]).to_string().parse().unwrap(), 65_536, 64).await.unwrap();
            let owner = wire::Authority { kind: wire::AggregateKind::Task as i32, id: "task".into() };
            let turn = OperationId::from_bytes([13;16]);
            let mut views = ClientViews::default();
            let mut live = ClientRenderer::default();
            for (chunk, visible) in chunks {
                views.model_event(&owner, turn, ModelEvent::Reasoning { delta: chunk }).unwrap();
                files.write_state(CommandFileRoot::Config, "settings.json", &serde_json::json!({"reasoning":visible})).await.unwrap();
                live.update(&views, &owner);
                let mut replay = ClientRenderer::default();
                replay.update(&views, &owner);
                let mut pictures = Vec::new();
                for renderer in [&mut live, &mut replay] {
                    let screen = renderer.screen_for(&files).unwrap();
                    if !visible { proptest::prop_assert!(screen.reasoning.is_empty()); }
                    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
                    terminal.draw(|frame| { screen.render(frame, &PromptInput::default(), &mut Viewport::default(), 0, 0); }).unwrap();
                    pictures.push(terminal.backend().buffer().clone());
                }
                proptest::prop_assert_eq!(&pictures[0], &pictures[1]);
            }
            Ok(())
        })?;
    }
}

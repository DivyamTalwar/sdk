use acyclic_harness::{
    OperationId,
    client::{
        ClientViews, CommandFileRoot, CommandSession, DEFAULT_COMMANDS_JSON, handle_tui_input,
    },
    task_volume::TaskVolume,
    tui::{
        Action, ClientRenderer, InputEffect, PromptInput, Viewport,
        ratatui::{Terminal, backend::TestBackend},
    },
    wire,
};
use acyclic_stream::{MemoryStream, StreamPath};
use std::sync::Arc;
struct NoFiles;
impl acyclic_harness::conversation::ContentResidencyVerifier for NoFiles {}

fn paint(
    renderer: &mut ClientRenderer,
    files: &CommandSession<MemoryStream>,
    width: u16,
    height: u16,
) -> String {
    let screen = renderer.screen_for(files).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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
    super::screen_text(&terminal)
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    // Property: each catalog/keymap publication updates modal help identically
    // on a live renderer and replay; Escape preserves the original transcript.
    #[test]
    fn help_follows_catalog_and_effective_keymap(sequence in proptest::collection::vec(proptest::bool::ANY, 1..5)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let volume = Arc::new(TaskVolume::open(Arc::new(MemoryStream::default()), StreamPath::new("help").unwrap()).await.unwrap());
            let mut files = CommandSession::open(volume, OperationId::from_bytes([3;16]).to_string().parse().unwrap(), 65_536, 64).await.unwrap();
            let owner = wire::Authority { kind: wire::AggregateKind::Task as i32, id: "task".into() };
            let mut views = ClientViews::default();
            views.model_event(&owner, OperationId::from_bytes([2;16]), acyclic_harness::model::ModelEvent::Content { delta: "Original transcript".into() }).unwrap();
            let mut live = ClientRenderer::default();
            live.update(&views, &owner);
            for (index, enabled) in sequence.into_iter().enumerate() {
                let mut catalog: serde_json::Value = serde_json::from_slice(DEFAULT_COMMANDS_JSON).unwrap();
                let description = format!("Followed command description {index}");
                catalog[0]["description"] = serde_json::json!(description);
                files.install_catalog(serde_json::to_vec(&catalog).unwrap()).await.unwrap();
                files.write_state(CommandFileRoot::Config, "keymap.json", &serde_json::json!({"Ctrl+l":if enabled { "Home" } else { "" }})).await.unwrap();
                handle_tui_input(&mut files, &NoFiles, InputEffect::Submit { text: "/help".into(), queued: false, edit: None }).await.unwrap();
                let visible = paint(&mut live, &files, 140, 180);
                proptest::prop_assert!(visible.contains("Shortcuts"));
                proptest::prop_assert!(visible.contains(&description));
                proptest::prop_assert_eq!(visible.contains("Ctrl+l · Line start"), enabled);
                proptest::prop_assert!(!visible.contains("Original transcript"));
                let mut replayed = ClientRenderer::default();
                replayed.update(&views, &owner);
                proptest::prop_assert_eq!(paint(&mut replayed, &files, 140, 180), visible);
                // Tiny resizes keep the prompt/layout valid while the modal is open.
                paint(&mut live, &files, 12, 4);
                handle_tui_input(&mut files, &NoFiles, InputEffect::Host(Action::Escape)).await.unwrap();
                proptest::prop_assert!(paint(&mut live, &files, 80, 20).contains("Original transcript"));
                proptest::prop_assert!(live.screen_for(&files).unwrap().overlay.is_empty());
            }
            Ok(())
        })?;
    }
}

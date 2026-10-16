use acyclic_harness::{
    OperationId,
    client::{CommandFileRoot, CommandSession, PromptFiles},
    task_volume::TaskVolume,
    tui::{
        Editor, PromptInput,
        key_event::{KeyCode, KeyEvent, KeyModifiers},
    },
};
use acyclic_stream::{MemoryStream, StreamPath};
use std::sync::Arc;

async fn session() -> CommandSession<MemoryStream> {
    let volume = Arc::new(
        TaskVolume::open(
            Arc::new(MemoryStream::default()),
            StreamPath::new("followed-prompt").unwrap(),
        )
        .await
        .unwrap(),
    );
    CommandSession::open(
        volume,
        OperationId::from_bytes([6; 16])
            .to_string()
            .parse()
            .unwrap(),
        65_536,
        64,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn unchanged_files_preserve_navigation_and_invalid_updates_are_atomic() {
    let mut files = session().await;
    files
        .write_state(
            CommandFileRoot::Config,
            "history.json",
            &serde_json::json!(["first", "second"]),
        )
        .await
        .unwrap();
    files
        .write_state(
            CommandFileRoot::Session,
            "draft.json",
            &serde_json::json!("draft"),
        )
        .await
        .unwrap();
    let mut input = PromptInput::default();
    let mut projection = PromptFiles::default();
    projection.refresh(&files, &mut input).unwrap();
    let generation = input.history().generation();
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    input.editor.set_text("", 0);
    input.key(up, 1, 78);
    assert_eq!(input.editor.text(), "second");
    for _ in 0..3 {
        projection.refresh(&files, &mut input).unwrap();
        assert_eq!(input.editor.text(), "second");
        assert_eq!(input.history().generation(), generation);
    }
    input.key(up, 2, 78);
    assert_eq!(input.editor.text(), "first");
    files
        .write_state(
            CommandFileRoot::Session,
            "draft.json",
            &serde_json::json!("external draft"),
        )
        .await
        .unwrap();
    files
        .write_state(
            CommandFileRoot::Config,
            "history.json",
            &serde_json::json!(false),
        )
        .await
        .unwrap();
    assert!(projection.refresh(&files, &mut input).is_err());
    assert_eq!(input.editor.text(), "first");
    assert_eq!(input.history().generation(), generation);
    files
        .write_state(
            CommandFileRoot::Config,
            "history.json",
            &serde_json::json!(["external history"]),
        )
        .await
        .unwrap();
    projection.refresh(&files, &mut input).unwrap();
    assert_eq!(input.editor.text(), "external draft");
    assert_eq!(input.history().entries(), &["external history"]);
    let mut terminal = super::Terminal::new(super::TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            acyclic_harness::tui::Screen::default().render(
                frame,
                &input,
                &mut super::Viewport::default(),
                0,
                0,
            );
        })
        .unwrap();
    assert!(super::screen_text(&terminal).contains("external draft"));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    // Property: saves append only local submissions, retain intervening external
    // history, normalize text, suppress adjacent duplicates and survive reopen.
    #[test]
    fn history_save_preserves_followed_entries(sequence in proptest::collection::vec((".{0,12}", ".{0,12}"), 1..6)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut files = session().await;
            let mut projection = PromptFiles::default();
            let mut input = PromptInput::default();
            projection.refresh(&files, &mut input).unwrap();
            let mut expected = acyclic_harness::tui::PromptHistory::default();
            for (local, remote) in sequence {
                let normalized = Editor::new(&local).text().to_owned();
                let previous = expected.entries().last().cloned();
                input.editor.set_text(&local, 0);
                input.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 1, 78);
                expected.push(remote);
                files.write_state(CommandFileRoot::Config, "history.json", &serde_json::json!(expected.entries())).await.unwrap();
                if !normalized.trim().is_empty() && previous.as_ref() != Some(&normalized) {
                    expected.push(normalized);
                }
                projection.save(&mut files, &mut input).await.unwrap();
                projection.refresh(&files, &mut input).unwrap();
                proptest::prop_assert_eq!(input.history().entries(), expected.entries());
                let before = input.history().generation();
                projection.save(&mut files, &mut input).await.unwrap();
                projection.refresh(&files, &mut input).unwrap();
                proptest::prop_assert_eq!(input.history().generation(), before);
                let mut reopened = PromptInput::default();
                PromptFiles::default().refresh(&files, &mut reopened).unwrap();
                proptest::prop_assert_eq!(reopened.history().entries(), expected.entries());
            }
            Ok(())
        })?;
    }
}

#![no_main]
use acyclic_harness::{
    client::{CommandFileRoot, CommandSession, PromptFiles, restore_tui_prompt, tui_scroll},
    task_volume::{FileKey, Query, TaskVolume, Write},
    tui::{ClientRenderer, Editor, InputKeymap, PromptInput},
};
use acyclic_stream::{MemoryStream, StreamPath};
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    let mut fields = data.splitn(5, |byte| *byte == 0);
    let draft = fields.next().unwrap_or_default();
    let Some(history) = fields.next() else {
        return;
    };
    let keymap = fields.next();
    let notifications = fields.next();
    let layout = fields.next();
    let expected_keymap = keymap.map_or(Some(InputKeymap::default()), |bytes| {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| InputKeymap::from_json(text).ok())
    });
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let volume = Arc::new(
                TaskVolume::open(
                    Arc::new(MemoryStream::default()),
                    StreamPath::new("prompt-fuzz").unwrap(),
                )
                .await
                .unwrap(),
            );
            let session = acyclic_harness::OperationId::from_bytes([9; 16]).to_string();
            let mut writes = vec![
                (format!(".ui/session-{session}/draft.json"), draft),
                (".ui/config/history.json".into(), history),
            ];
            if let Some(bytes) = keymap {
                writes.push((".ui/config/keymap.json".into(), bytes));
            }
            if let Some(bytes) = notifications {
                writes.push((format!(".ui/session-{session}/notifications.json"), bytes));
            }
            if let Some(bytes) = layout {
                writes.push((format!(".ui/session-{session}/layout.json"), bytes));
            }
            for (path, bytes) in writes {
                let cut = volume.snapshot(&Query::Root, 64).await.unwrap().generation;
                volume
                    .append(
                        cut,
                        &Write::new(FileKey::parse(&path).unwrap(), bytes.to_vec()),
                    )
                    .await
                    .unwrap();
            }
            let Ok(mut files) =
                CommandSession::open(volume, session.parse().unwrap(), 65_536, 64).await
            else {
                return;
            };
            if let Some(bytes) = layout {
                let expected = serde_json::from_slice::<serde_json::Value>(bytes)
                    .ok()
                    .and_then(|value| {
                        if !value.is_object() {
                            return None;
                        }
                        value.get("transcript_back").map_or(Some(0), |value| {
                            value.as_u64().and_then(|rows| usize::try_from(rows).ok())
                        })
                    });
                assert_eq!(tui_scroll(&files).ok(), expected);
            }
            let mut renderer = ClientRenderer::default();
            let result = renderer.screen_for(&files);
            let expected: Result<Vec<String>, _> =
                serde_json::from_slice(notifications.unwrap_or(b"[]"));
            match expected {
                Err(_) => assert!(result.is_err()),
                Ok(messages) => {
                    let screen = result.unwrap();
                    assert_eq!(screen.notifications.len(), messages.len());
                    for (row, message) in screen.notifications.iter().zip(messages) {
                        let text: String =
                            row.spans.iter().map(|span| span.content.as_ref()).collect();
                        assert_eq!(text, message.escape_debug().to_string());
                        assert!(!text.chars().any(char::is_control));
                    }
                }
            }
            let mut input = PromptInput::new(Editor::new("original"));
            let mut projected = input.clone();
            let mut live = PromptFiles::default();
            let refreshed = live.refresh(&files, &mut projected);
            let restored = restore_tui_prompt(&files, &mut input);
            if restored.is_err() || expected_keymap.is_none() {
                assert!(refreshed.is_err());
                assert_eq!(projected.editor.text(), "original");
                assert!(projected.history().entries().is_empty());
                assert_eq!(projected.keymap, InputKeymap::default());
                return;
            }
            files
                .write_state(
                    CommandFileRoot::Session,
                    "layout.json",
                    &serde_json::json!({"open_view":"shortcuts"}),
                )
                .await
                .unwrap();
            if expected_keymap.is_some() && notifications.is_none() {
                let help = renderer.screen_for(&files).unwrap();
                assert!(!help.overlay.is_empty());
                for row in help.overlay {
                    assert!(
                        row.spans
                            .iter()
                            .all(|span| !span.content.chars().any(char::is_control))
                    );
                }
            }
            refreshed.unwrap();
            assert_eq!(projected.editor.text(), input.editor.text());
            assert_eq!(projected.history().entries(), input.history().entries());
            assert_eq!(Some(&projected.keymap), expected_keymap.as_ref());
            let generation = projected.history().generation();
            live.refresh(&files, &mut projected).unwrap();
            assert_eq!(projected.history().generation(), generation);
            live.save(&mut files, &mut projected).await.unwrap();
            let mut replayed = PromptInput::default();
            PromptFiles::default()
                .refresh(&files, &mut replayed)
                .unwrap();
            assert_eq!(replayed.editor.text(), projected.editor.text());
            assert_eq!(replayed.history().entries(), projected.history().entries());
            assert_eq!(replayed.keymap, projected.keymap);
        });
});

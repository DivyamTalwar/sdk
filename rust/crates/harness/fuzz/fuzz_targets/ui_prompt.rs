#![no_main]
use acyclic_harness::{
    client::{CommandSession, PromptFiles, restore_tui_prompt},
    task_volume::{FileKey, Query, TaskVolume, Write},
    tui::{Editor, InputKeymap, PromptInput},
};
use acyclic_stream::{MemoryStream, StreamPath};
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    let mut fields = data.splitn(3, |byte| *byte == 0);
    let draft = fields.next().unwrap_or_default();
    let Some(history) = fields.next() else {
        return;
    };
    let keymap = fields.next();
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

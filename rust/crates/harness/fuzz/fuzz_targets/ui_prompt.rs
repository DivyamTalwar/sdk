#![no_main]
use acyclic_harness::{
    client::{CommandSession, restore_tui_prompt, save_tui_draft},
    task_volume::{FileKey, Query, TaskVolume, Write},
    tui::{Editor, PromptInput},
};
use acyclic_stream::{MemoryStream, StreamPath};
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    let Some(split) = data.iter().position(|byte| *byte == 0) else {
        return;
    };
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
            let session = acyclic_harness::OperationId::from_bytes([9; 16])
                .into_bytes()
                .into();
            for (path, bytes) in [
                (format!(".ui/session-{session}/draft.json"), &data[..split]),
                (".ui/config/history.json".into(), &data[split + 1..]),
            ] {
                let cut = volume.snapshot(&Query::Root, 64).await.unwrap().generation;
                volume
                    .append(
                        cut,
                        &Write::new(FileKey::parse(&path).unwrap(), bytes.to_vec()),
                    )
                    .await
                    .unwrap();
            }
            let Ok(mut files) = CommandSession::open(volume, session, 65_536, 64).await else {
                return;
            };
            let mut input = PromptInput::new(Editor::new("original"));
            match restore_tui_prompt(&files, &mut input) {
                Err(_) => {
                    assert_eq!(input.editor.text(), "original");
                    assert!(input.history().entries().is_empty());
                }
                Ok(()) => {
                    save_tui_draft(&mut files, &input).await.unwrap();
                    let mut replayed = PromptInput::default();
                    restore_tui_prompt(&files, &mut replayed).unwrap();
                    assert_eq!(replayed.editor.text(), input.editor.text());
                    assert_eq!(replayed.history().entries(), input.history().entries());
                }
            }
        });
});

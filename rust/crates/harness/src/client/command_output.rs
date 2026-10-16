//! Projection of the existing command outcome envelope, without another store.
use crate::task_volume::FileValue;
use serde::Deserialize;

#[derive(Deserialize)]
struct Outcome {
    sequence: u64,
    ok: Option<Text>,
}
#[derive(Deserialize)]
struct Text {
    #[serde(default)]
    text: Option<String>,
}

pub(super) fn text(file: &FileValue) -> Option<(u64, String)> {
    let outcome: Outcome = serde_json::from_slice(&file.bytes).ok()?;
    if outcome.sequence == 0 {
        return None;
    }
    Some((outcome.sequence, outcome.ok?.text?))
}

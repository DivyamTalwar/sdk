//! Projection of the existing command outcome envelope, without another store.
use super::HeadlessOutcome;
use crate::{Error, Result, task_volume::FileValue};
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Deserialize)]
struct Outcome<'a> {
    sequence: u64,
    completion: Option<HeadlessOutcome>,
    #[serde(borrow)]
    ok: Option<&'a RawValue>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
    #[serde(default)]
    completed: bool,
    #[serde(default)]
    output_omitted: bool,
}
#[derive(Deserialize)]
struct Text {
    #[serde(default)]
    text: Option<String>,
}

fn settled(outcome: &Outcome<'_>) -> Result<HeadlessOutcome> {
    let invalid = || Error::Invalid("invalid command outcome envelope".into());
    if outcome.sequence == 0 {
        return Err(invalid());
    }
    if outcome.completed && outcome.output_omitted {
        return Ok(outcome.completion.unwrap_or(HeadlessOutcome::Indeterminate));
    }
    match (outcome.ok.is_some(), outcome.error) {
        (true, None)
            if outcome
                .completion
                .is_none_or(|state| state == HeadlessOutcome::Completed) =>
        {
            Ok(HeadlessOutcome::Completed)
        }
        (false, Some(error))
            if error.get().starts_with('"')
                && outcome.completion != Some(HeadlessOutcome::Completed) =>
        {
            Ok(outcome.completion.unwrap_or(HeadlessOutcome::Indeterminate))
        }
        _ => Err(invalid()),
    }
}

pub(super) fn completion(bytes: &[u8]) -> Result<HeadlessOutcome> {
    let outcome: Outcome<'_> =
        serde_json::from_slice(bytes).map_err(|error| Error::Invalid(error.to_string()))?;
    settled(&outcome)
}

pub(super) fn text(file: &FileValue) -> Option<(u64, String)> {
    let outcome: Outcome<'_> = serde_json::from_slice(&file.bytes).ok()?;
    if settled(&outcome).ok()? != HeadlessOutcome::Completed {
        return None;
    }
    let text: Text = serde_json::from_str(outcome.ok?.get()).ok()?;
    Some((outcome.sequence, text.text?))
}

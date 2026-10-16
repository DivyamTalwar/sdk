//! Local wire acceptance rules from provider-input-rules.md.
use crate::Adapter;
use serde_json::Value;

mod anthropic;
mod chat;
mod gemini;
mod responses;

/// Checks the projected wire grammar locally, without I/O or credential access.
pub fn validate_input(adapter: Adapter, body: &Value) -> Vec<&'static str> {
    let mut errors = Vec::new();
    match adapter {
        Adapter::Anthropic => anthropic::validate(body, &mut errors),
        Adapter::Responses => responses::validate(body, &mut errors),
        Adapter::Chat => chat::validate(body, &mut errors),
        Adapter::Gemini => gemini::validate(body, &mut errors),
    }
    errors
}

fn check(ok: bool, rule: &'static str, errors: &mut Vec<&'static str>) {
    if !ok {
        errors.push(rule);
    }
}

fn name(value: &Value, maximum: usize, leading_letter: bool, punctuation: &[u8]) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty()
            && text.len() <= maximum
            && (!leading_letter
                || text.as_bytes()[0].is_ascii_alphabetic()
                || text.starts_with('_'))
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || punctuation.contains(&b))
    })
}
fn id(value: &Value, maximum: usize) -> bool {
    name(value, maximum, false, b"_-")
}
fn text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.trim().is_empty())
}
fn object_arguments(value: &Value) -> bool {
    value
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .is_some_and(|value| value.is_object())
}

#[cfg(test)]
mod tests;

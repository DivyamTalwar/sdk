use super::*;
use serde_json::json;

#[test]
fn small_events_cannot_accumulate_unbounded_calls_or_thinking() -> Result<()> {
    let mut state = State::new(256);
    let mut rejected = false;
    for index in 0..10_000 {
        let event = json!({"choices":[{"delta":{"tool_calls":[{"index":index,"id":format!("c{index}"),"function":{"name":"echo","arguments":"{}"}}]}}]});
        assert!(event.to_string().len() < 256);
        if chat::parse(&mut state, &event).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    assert!(state.calls.len() < 10_000);
    assert!(state.budget.used().0 <= 256);
    let before = state.calls.len();
    assert!(state.call(20_000).is_err());
    assert_eq!(
        state.calls.len(),
        before,
        "reject before allocating a call slot"
    );

    let mut state = State::new(1024 * 1024);
    let mut rejected = false;
    for index in 0..10_000 {
        let event = json!({"type":"content_block_start","index":index,"content_block":{"type":"thinking","thinking":"","signature":""}});
        assert!(event.to_string().len() < 256);
        if anthropic::parse(&mut state, &event).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    assert!(state.thinking.len() < 10_000);
    assert!(
        state.budget.used().0 < 1024 * 1024,
        "item bound fires independently of bytes"
    );
    assert!(state.budget.used().1 <= 1024);
    Ok(())
}

#[test]
fn replacements_and_usage_updates_release_their_old_budget() -> Result<()> {
    let mut state = State::new(1024);
    let usage = json!({"input_tokens":10,"output_tokens":1});
    state.usage(&usage)?;
    let retained = state.budget.used();
    for _ in 0..10_000 {
        state.usage(&usage)?;
    }
    assert_eq!(state.budget.used(), retained);
    let item = json!({"type":"reasoning","encrypted_content":"opaque","summary":[]});
    state.retain_thinking(0, &item)?;
    let retained = state.budget.used();
    for _ in 0..10_000 {
        state.retain_thinking(0, &item)?;
    }
    assert_eq!(state.budget.used(), retained);
    Ok(())
}

#[test]
fn cumulative_fragments_and_json_payloads_are_admitted_before_growth() -> Result<()> {
    let mut state = State::new(256);
    let (call, budget) = state.call(0)?;
    budget.string(&mut call.id, "call")?;
    let fragment = Value::String("x".repeat(100));
    budget.append(&mut call.arguments, Some(&fragment))?;
    let before = (call.arguments.len(), call.arguments.capacity());
    assert!(budget.append(&mut call.arguments, Some(&fragment)).is_err());
    assert_eq!((call.arguments.len(), call.arguments.capacity()), before);
    let signature = json!({"blocks":["x".repeat(256)]});
    assert!(budget.value(&mut call.metadata, &signature).is_err());
    assert!(call.metadata.is_null());
    Ok(())
}

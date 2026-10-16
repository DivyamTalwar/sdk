use super::*;
use proptest::prelude::*;

#[test]
fn malformed_tool_answers_cannot_implicitly_allow_native_execution() -> Result<()> {
    for host in [
        ExternalHost::Codex,
        ExternalHost::ClaudeCode,
        ExternalHost::Pi,
    ] {
        let event = if host == ExternalHost::Pi {
            "tool_call"
        } else {
            "PreToolUse"
        };
        for value in [
            Value::Null,
            json!([]),
            json!(true),
            json!({}),
            json!({"block":"false"}),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse"}}),
            json!({"hookSpecificOutput":{"hookEventName":"Other","permissionDecision":"allow"}}),
        ] {
            assert!(
                hook_answer(
                    host,
                    event,
                    &serde_json::to_vec(&value).map_err(|e| invalid(&e))?
                )
                .is_err()
            );
        }
        let denied = hook_failure(host.name(), event, "rejected");
        let bytes = serde_json::to_vec(&denied).map_err(|e| invalid(&e))?;
        assert_eq!(hook_answer(host, event, &bytes)?, denied);
    }
    Ok(())
}

proptest! {
    // Property: a durable failure is never interpreted as native permission or
    // a successful lifecycle acknowledgement, for any catalogued host event.
    #[test]
    fn failed_results_reject_every_native_hook(message in ".{0,64}") {
        let answer = serde_json::to_vec(&acyclic_harness::Outcome::<Value>::Failed {message})?;
        for host in [ExternalHost::Codex, ExternalHost::ClaudeCode, ExternalHost::Pi] {
            for spec in hooks(host) {
                prop_assert!(hook_answer(host, spec.event, &answer).is_err());
            }
        }
    }

    // Property: explicit native decisions and their rewritten inputs round trip
    // exactly; deleting the decision always rejects the answer.
    #[test]
    fn native_decision_round_trip_never_defaults_to_allow(
        host in prop_oneof![Just(ExternalHost::Codex), Just(ExternalHost::ClaudeCode), Just(ExternalHost::Pi)],
        permit in any::<bool>(), reason in ".{0,64}", command in ".{0,64}",
    ) {
        let (event, mut answer) = if host == ExternalHost::Pi {
            ("tool_call", json!({"block":!permit,"reason":reason,"updatedInput":{"command":command}}))
        } else {
            ("PreToolUse", json!({"hookSpecificOutput":{"hookEventName":"PreToolUse",
                "permissionDecision":if permit {"allow"} else {"deny"},
                "permissionDecisionReason":reason,"updatedInput":{"command":command}}}))
        };
        prop_assert_eq!(hook_answer(host, event, &serde_json::to_vec(&answer)?)?, answer.clone());
        let target = if host == ExternalHost::Pi {
            answer.as_object_mut()
        } else {
            answer.get_mut("hookSpecificOutput").and_then(Value::as_object_mut)
        }.ok_or_else(|| Error::Invalid("missing fixture object".into()))?;
        target.remove(if host == ExternalHost::Pi {"block"} else {"permissionDecision"});
        prop_assert!(hook_answer(host, event, &serde_json::to_vec(&answer)?).is_err());
    }
}

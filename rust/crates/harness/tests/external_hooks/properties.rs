use super::*;
use acyclic_harness::task_volume::Component;
use proptest::prelude::*;

fn scenario(index: usize, session: &str) -> Result<Scenario> {
    let scenarios: Vec<Scenario> = serde_json::from_str(include_str!("../external_hooks.json"))
        .map_err(|error| invalid(&error))?;
    let index = index
        .checked_rem(scenarios.len())
        .ok_or_else(|| Error::Invalid("hook scenarios are empty".into()))?;
    let mut selected = scenarios
        .into_iter()
        .nth(index)
        .ok_or_else(|| Error::Invalid("hook scenario is absent".into()))?;
    let field = if selected.host == ExternalHost::Pi {
        "sessionId"
    } else {
        "session_id"
    };
    set(&mut selected.input, field, json!(session))?;
    Ok(selected)
}

proptest! {
    // Property: every native hook normalizes into a valid immutable request
    // file and round trips without retaining unrelated native metadata.
    #[test]
    fn native_requests_round_trip_through_request_files(
        index in any::<usize>(),
        session in "[a-zA-Z0-9_-]{1,24}",
        metadata in ".{0,64}",
    ) {
        let mut selected = scenario(index, &session)?;
        let expected = request(selected.host, &selected.event, selected.input.clone())?;
        set(&mut selected.input, "environment", json!({"private": metadata}))?;
        set(&mut selected.input, "transcript_path", json!("/private/transcript"))?;
        let actual = request(selected.host, &selected.event, selected.input)?;
        prop_assert_eq!(&actual, &expected);
        let task = Component::new("task")?;
        let file = volume::file(&task, &actual)?;
        prop_assert_eq!(ExternalRequest::from_file(&task, &file)?, actual);
        prop_assert!(ExternalRequest::from_file(&Component::new("other-task")?, &file).is_err());
    }

    // Property: a tool retry keeps its operation despite changed arguments,
    // while native session identity remains part of the operation namespace.
    #[test]
    fn tool_retry_identity_is_independent_of_arguments(
        host in prop_oneof![Just(ExternalHost::Codex), Just(ExternalHost::ClaudeCode), Just(ExternalHost::Pi)],
        session in "[a-zA-Z0-9_-]{1,24}",
        call in "[a-zA-Z0-9_-]{1,24}",
        command in ".{0,64}",
    ) {
        let (event, mut input) = if host == ExternalHost::Pi {
            ("tool_call", json!({"sessionId":session,"toolCallId":call,"toolName":"bash","input":{"command":command}}))
        } else {
            ("PreToolUse", json!({"session_id":session,"tool_use_id":call,"tool_name":"Bash","tool_input":{"command":command}}))
        };
        let original = request(host, event, input.clone())?;
        let field = if host == ExternalHost::Pi { "input" } else { "tool_input" };
        set(&mut input, field, json!({"command":format!("{command} changed")}))?;
        let changed = request(host, event, input.clone())?;
        prop_assert_eq!(changed.operation_id, original.operation_id);
        prop_assert_ne!(changed.action, original.action);
        let field = if host == ExternalHost::Pi { "sessionId" } else { "session_id" };
        set(&mut input, field, json!(format!("{session}-other")))?;
        prop_assert_ne!(request(host, event, input)?.operation_id, original.operation_id);
    }

    // Property: two native spellings of the same identity field are rejected,
    // even when their values happen to agree.
    #[test]
    fn native_identity_aliases_cannot_be_ambiguous(
        index in any::<usize>(),
        session in "[a-zA-Z0-9_-]{1,24}",
    ) {
        let mut selected = scenario(index, &session)?;
        let alias = if selected.host == ExternalHost::Pi { "session_id" } else { "sessionId" };
        set(&mut selected.input, alias, json!(session))?;
        prop_assert!(request(selected.host, &selected.event, selected.input).is_err());
    }
}

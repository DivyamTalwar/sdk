use super::*;
use crate::external_executor::{ExternalHost, request};
use proptest::prelude::*;
use serde_json::json;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    // The typed command projection is canonical, retains the native operation,
    // and rejects any field that could change its authority or executor kind.
    #[test]
    fn stock_commands_round_trip_without_accepting_an_executor_override(
        sequence in any::<u64>(),
        id in any::<[u8;16]>(),
        choice in 0usize..4,
        input in "[a-z]{0,32}",
    ) {
        let operation = crate::OperationId::from_bytes(id);
        let (name, arguments, kind) = match choice {
            0 => ("acyclic_spawn", json!({"name":"acyclic.command_session","version":"1","input":input}), TASK_ADMIT_COMMAND_KIND),
            1 => ("acyclic_receive", json!({"after":sequence}), MAIL_RECEIVE_TASK_COMMAND_KIND),
            2 => ("acyclic_wait", json!({"admission_operation":operation}), TASK_OBSERVE_COMMAND_KIND),
            _ => ("acyclic_cancel", json!({"child":crate::TaskId::from_bytes(id)}), TASK_CANCEL_COMMAND_KIND),
        };
        for host in [ExternalHost::Codex, ExternalHost::ClaudeCode, ExternalHost::Pi] {
            let call = request(host, if host == ExternalHost::Pi {"tool_call"} else {"PreToolUse"},
                json!({"sessionId":"commands","toolCallId":"call","toolName":name,"input":arguments}))?;
            prop_assert_eq!(call.task_command()?, Some((kind, arguments.clone())));
            let tools = crate::tool::ToolRegistry::default();
            let validate = super::super::commands::validate_command;
            prop_assert_eq!(validate(&call, kind, &arguments, &tools)?, None);
            prop_assert!(validate(&call, TOOL_TASK_COMMAND_KIND, &arguments, &tools).is_err());
            let mut forged = arguments.clone();
            forged["kind"] = json!(TOOL_TASK_COMMAND_KIND);
            prop_assert!(validate(&call, kind, &forged, &tools).is_err());
            let encoded = serde_json::to_vec(&call).unwrap();
            let mut replay: ExternalRequest = serde_json::from_slice(&encoded).unwrap();
            prop_assert_eq!(replay.task_command()?, call.task_command()?);
            prop_assert_eq!(replay.operation_id, call.operation_id);
            if let ExternalAction::RunTool {arguments, ..} = &mut replay.action {
                arguments["kind"] = json!(TOOL_TASK_COMMAND_KIND);
            }
            prop_assert!(replay.task_command().is_err());
        }
    }
}

#[test]
fn every_exposed_command_has_a_typed_preparation() -> Result<()> {
    for definition in crate::external_executor::command_catalog::definitions() {
        let error = request(
            ExternalHost::Pi,
            "tool_call",
            json!({
                "sessionId":"catalog", "toolCallId":"call", "toolName":definition.name,"input":{}
            }),
        )?
        .task_command();
        assert!(error.is_err(), "command must validate its required fields");
        assert_eq!(definition.input_schema["additionalProperties"], false);
    }
    Ok(())
}

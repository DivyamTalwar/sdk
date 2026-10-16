use super::*;

#[test]
fn canonical_jsonl_golden_frames() {
    let frame = wire::ServerFrame {
        frame: Some(wire::server_frame::Frame::Admission(wire::Admission {
            operation: Some(wire::OperationIdentity {
                operation_id: "op".into(),
                idempotency_key: "key".into(),
            }),
            state: wire::AdmissionState::Accepted as i32,
            error: None,
        })),
    };
    let line = jsonl::server_frame(&frame).unwrap();
    assert_eq!(
        line,
        r#"{"admission":{"operation":{"operationId":"op","idempotencyKey":"key"},"state":"ADMISSION_STATE_ACCEPTED"}}"#
    );
    assert_eq!(jsonl::parse_server_frame(&line).unwrap(), frame);
    let line = r#"{"acknowledge":{"authority":{"kind":"AGGREGATE_KIND_TASK","id":"task"},"generation":"g","throughRevision":"18446744073709551615"}}"#;
    let frame = jsonl::parse_client_frame(line).unwrap();
    assert_eq!(jsonl::client_frame(&frame).unwrap(), line);
    assert!(jsonl::parse_client_frame(r#"{"bogus":1}"#).is_err());
    assert!(jsonl::parse_client_frame("{} {}").is_err());
}

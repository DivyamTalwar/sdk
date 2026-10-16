//! The generated accepted case and rejected mutations prevent a vacuous oracle.
use acyclic_harness::Result;
use serde_json::{Value, json};

pub fn exercise(input: &[u8], admit: fn(&[u8]) -> Result<()>) {
    let command = json!({
        "operationId": "operation", "authority": { "kind": "task", "id": "task" },
        "kind": "task.submit", "offlineSafe": true,
        "payload": { "metadata": { "number": input.len(), "ready": true } }
    });
    let encoded = serde_json::to_vec(&command).unwrap();
    admit(&encoded).expect("reference-only generated command must be accepted");
    for value in [false, true] {
        let mut unsafe_command = command.clone();
        if value {
            unsafe_command["kind"] = json!("interaction.resolve.approval");
        } else {
            unsafe_command["offlineSafe"] = json!(false);
        }
        assert!(
            admit(&serde_json::to_vec(&unsafe_command).unwrap()).is_err(),
            "online-only command entered offline storage"
        );
    }
    for key in [
        "token",
        "Authorization",
        "bearer",
        "credential",
        "secret",
        "password",
        "api-key",
        "apikey",
        "user_scope",
        "proof",
        "body",
        "text",
        "bytes",
        "base64",
        "data",
    ] {
        let mut unsafe_command = command.clone();
        unsafe_command["payload"]["metadata"][key] = json!(1);
        assert!(
            admit(&serde_json::to_vec(&unsafe_command).unwrap()).is_err(),
            "credential or body field entered offline storage"
        );
    }
    for (key, value) in [
        ("content", json!("inline prompt")),
        ("content", Value::Null),
        ("attachments", json!([1, 2, 3])),
        ("attachments", Value::Null),
        ("references", json!([{"path":"unversioned"}])),
        ("artifacts", json!([false])),
        ("metadata", json!({"hint":"free form"})),
        ("body", json!("inline")),
    ] {
        let mut unsafe_command = command.clone();
        unsafe_command["payload"][key] = value;
        assert!(
            admit(&serde_json::to_vec(&unsafe_command).unwrap()).is_err(),
            "non-reference content entered offline storage"
        );
    }
    for key in ["bearer", "scope", "credential", "unexpected"] {
        let mut unsafe_command = command.clone();
        unsafe_command[key] = json!(true);
        assert!(
            admit(&serde_json::to_vec(&unsafe_command).unwrap()).is_err(),
            "unsupported command field admitted"
        );
    }
    let mut unsafe_command = command.clone();
    unsafe_command["authority"]["id"] = json!("../outside");
    assert!(admit(&serde_json::to_vec(&unsafe_command).unwrap()).is_err());
    unsafe_command = command;
    unsafe_command
        .as_object_mut()
        .unwrap()
        .remove("offlineSafe");
    assert!(admit(&serde_json::to_vec(&unsafe_command).unwrap()).is_err());
    if admit(input).is_ok() {
        let decoded: Value = serde_json::from_slice(input).expect("admission requires JSON");
        let encoded = serde_json::to_vec(&decoded).unwrap();
        admit(&encoded).expect("canonical round-trip preserves admitted requests");
        assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), decoded);
    }
}

#[cfg(test)]
pub fn broken_allow_all(_: &[u8]) -> Result<()> {
    Ok(())
}

#[cfg(test)]
pub fn broken_reject_all(_: &[u8]) -> Result<()> {
    Err(acyclic_harness::Error::Invalid("broken admission".into()))
}

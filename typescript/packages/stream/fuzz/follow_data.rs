use serde_json::Value;

fn number(value: &Value, field: &str) -> Option<u64> {
    value[field].as_str()?.parse().ok()
}

// Independent wire invariant, shared by shrinking properties and fuzz replay.
// The JS driver reports decoded fields, never its own validity verdict.
pub fn accepts(value: &Value, delivery: u8, next: u64) -> bool {
    let Some(after) = number(value, "after") else {
        return false;
    };
    let Some(accepted) = number(value, "accepted") else {
        return false;
    };
    let Some(durable) = number(value, "durable") else {
        return false;
    };
    if after <= next || value["opaque"] != 0 || accepted < after || durable > accepted {
        return false;
    }
    match (delivery, value["item"].as_str()) {
        (1, Some("record")) => {
            number(value, "sequence") == Some(next)
                && next.checked_add(1) == Some(after)
                && value["commit"] == 32
                && value["size"].as_u64().is_some_and(|size| size <= 65_536)
        }
        (2, Some("latestValue")) => value["key"].as_u64().is_some_and(|size| size > 0),
        _ => false,
    }
}

pub fn verify(value: &Value, delivery: u8, next: u64) {
    if let Some(confirmation) = value["confirmation"].as_array() {
        let cursor_matches = value["resume"] == "1" && value["opaque"] == 0;
        let confirmed = confirmation.len() == 32 && confirmation.iter().all(|byte| byte == 7);
        assert_eq!(
            value["granted"],
            cursor_matches && confirmed,
            "checked follow credit requires the exact anchor: {value}"
        );
        assert_eq!(
            value["outcome"],
            if !cursor_matches {
                "invalid_response"
            } else if confirmed {
                "pending"
            } else {
                "unsupported"
            },
            "checked confirmation changed the contract: {value}"
        );
        return;
    }
    if value.get("outcome").is_some() {
        assert_eq!(
            value["outcome"] == "data",
            accepts(value, delivery, next),
            "follow data violated the independent wire invariant: {value}"
        );
    }
}

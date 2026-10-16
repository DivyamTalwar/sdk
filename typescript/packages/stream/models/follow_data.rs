use proptest::prelude::*;
use serde_json::json;
use std::path::Path;
#[path = "../fuzz/driver.rs"]
mod driver;
#[path = "../fuzz/follow_data.rs"]
mod oracle;
#[path = "schedules.rs"]
mod schedules;

fn driver(fault: &str) -> driver::Driver {
    driver::Driver::configured(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.."),
        "follow-data",
        fault,
    )
}
fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
    out
}
fn bytes(field: u64, value: &[u8]) -> Vec<u8> {
    [
        varint(field * 8 + 2),
        varint(value.len() as u64),
        value.to_vec(),
    ]
    .concat()
}
fn number(field: u64, value: u64) -> Vec<u8> {
    [varint(field * 8), varint(value)].concat()
}
#[derive(Clone, Debug)]
struct Case {
    delivery: u8,
    next: u64,
    after: u64,
    accepted: u64,
    durable: u64,
    opaque: Vec<u8>,
    record: bool,
    sequence: u64,
    commit: Vec<u8>,
    key: Vec<u8>,
    value: Vec<u8>,
}
impl Default for Case {
    fn default() -> Self {
        Self {
            delivery: 2,
            next: 0,
            after: 9,
            accepted: 10,
            durable: 8,
            opaque: vec![],
            record: false,
            sequence: 0,
            commit: vec![7; 32],
            key: vec![1],
            value: vec![42],
        }
    }
}
impl Case {
    fn frame(&self) -> Vec<u8> {
        let cursor = [number(1, self.after), bytes(2, &self.opaque)].concat();
        let item = if self.record {
            bytes(
                10,
                &[
                    number(1, self.sequence),
                    bytes(2, &self.value),
                    bytes(3, &self.commit),
                ]
                .concat(),
            )
        } else {
            bytes(
                15,
                &[bytes(1, &self.key), bytes(2, &self.value), number(3, 1)].concat(),
            )
        };
        bytes(
            15,
            &[
                bytes(1, &cursor),
                number(2, self.accepted),
                number(3, self.durable),
                item,
            ]
            .concat(),
        )
    }
    fn check(&self, driver: &mut driver::Driver) {
        let hex: String = self
            .frame()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let result = driver.call(json!({ "action":"decode", "hex":hex, "delivery":self.delivery, "from":self.next.to_string() }));
        assert_eq!(result["after"], self.after.to_string());
        assert_eq!(result["accepted"], self.accepted.to_string());
        assert_eq!(result["durable"], self.durable.to_string());
        assert_eq!(result["opaque"], self.opaque.len());
        oracle::verify(&result, self.delivery, self.next);
    }
}

proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 128,
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(concat!(env!("CARGO_MANIFEST_DIR"), "/proptest-regressions.txt")))),
        ..Default::default() })]
    #[test]
    fn follow_data_round_trip_enforces_the_independent_cursor_and_watermark_contract(
        (next, after, accepted, durable, sequence) in (any::<u64>(), any::<u64>(), any::<u64>(), any::<u64>(), any::<u64>()),
        record in any::<bool>(), latest in any::<bool>(),
        opaque in proptest::collection::vec(any::<u8>(), 0..2),
        commit in proptest::collection::vec(any::<u8>(), 0..34),
        key in proptest::collection::vec(any::<u8>(), 0..4), value in proptest::collection::vec(any::<u8>(), 0..40),
    ) {
        static SHARED: std::sync::OnceLock<std::sync::Mutex<driver::Driver>> = std::sync::OnceLock::new();
        let shared = SHARED.get_or_init(|| std::sync::Mutex::new(driver("")));
        let mut real = shared.lock().unwrap();
        Case { delivery: if latest { 2 } else { 1 }, next, after, accepted, durable, opaque, record, sequence, commit, key, value }.check(&mut real);
        // Include valid frames and overflow edges rather than letting random
        // invalid watermarks dominate coverage of the acceptance branch.
        Case { next, after: next.saturating_add(1), accepted: u64::MAX, durable: next, ..Case::default() }.check(&mut real);
        Case { delivery: 1, record: true, sequence: next, next, after: next.saturating_add(1), accepted: u64::MAX, durable: next, ..Case::default() }.check(&mut real);
    }

    #[test]
    fn follow_modes_and_snapshot_guards_are_validated_before_admission(delivery in 0u8..5, next in 0u64..3, guard in 0usize..35) {
        static SHARED: std::sync::OnceLock<std::sync::Mutex<driver::Driver>> = std::sync::OnceLock::new();
        let shared = SHARED.get_or_init(|| std::sync::Mutex::new(driver("")));
        let result = shared.lock().unwrap().call(json!({"action":"validate", "delivery":delivery, "from":next.to_string(), "guard":guard}));
        let accepted = (delivery == 1 || delivery == 2) && (guard == 0 || (delivery == 1 && guard == 32 && next > 0));
        prop_assert_eq!(result["accepted"].as_bool(), Some(accepted));
        if !accepted { prop_assert_eq!(&result["code"], &json!("invalid_argument")); }
    }
}

fn confirmation(real: &mut driver::Driver, guard: &[u8]) {
    let input = bytes(14, &[bytes(1, &number(1, 1)), bytes(2, guard)].concat());
    let hex: String = input.iter().map(|byte| format!("{byte:02x}")).collect();
    let result = real.call(json!({"action":"decode", "hex":hex, "delivery":1, "from":"1"}));
    assert_eq!(result["confirmation"], json!(guard));
    oracle::verify(&result, 1, 1);
}
#[test]
fn checked_confirmations_require_the_exact_nonempty_anchor() {
    let mut real = driver("");
    for guard in [vec![], vec![7; 32], vec![9; 32]] {
        confirmation(&mut real, &guard);
    }
}
#[test]
#[should_panic(expected = "checked follow credit requires the exact anchor")]
fn negative_control_accepts_a_foreign_snapshot_anchor() {
    confirmation(&mut driver("confirmation"), &[9; 32]);
}
#[test]
#[should_panic(expected = "31-byte guard was accepted")]
fn negative_control_accepts_a_short_snapshot_guard() {
    let result = driver("shortGuard")
        .call(json!({"action":"validate", "delivery":1, "from":"1", "guard":31}));
    assert_eq!(result["accepted"], false, "31-byte guard was accepted");
}

#[test]
fn negative_controls_violate_the_same_wire_oracle() {
    for (fault, case) in [
        ("latestStrict", Case::default()),
        (
            "skipAll",
            Case {
                delivery: 1,
                record: true,
                ..Case::default()
            },
        ),
        (
            "emptyKey",
            Case {
                key: vec![],
                ..Case::default()
            },
        ),
        (
            "watermarks",
            Case {
                durable: 11,
                ..Case::default()
            },
        ),
        (
            "opaque",
            Case {
                opaque: vec![1],
                ..Case::default()
            },
        ),
        (
            "commit",
            Case {
                delivery: 1,
                record: true,
                after: 1,
                commit: vec![1; 31],
                ..Case::default()
            },
        ),
        (
            "wrongMode",
            Case {
                delivery: 1,
                after: 1,
                ..Case::default()
            },
        ),
        ("metadata", Case::default()),
    ] {
        let mut broken = driver(fault);
        let error =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| case.check(&mut broken)))
                .expect_err(&format!("undetected mutant {fault}"));
        let text = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap_or("");
        let expected = if fault == "metadata" {
            "raw follow changed data or watermarks"
        } else {
            "independent wire invariant"
        };
        assert!(
            text.contains(expected),
            "mutant {fault} failed outside its oracle: {text}"
        );
    }
}

#[test]
fn checked_in_seeds_decode_to_the_intended_cases() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fuzz/corpus/follow_data");
    let mut real = driver("");
    for (name, delivery, after, accepted, durable, item, key, commit, outcome) in [
        (
            "latest-jump",
            2,
            9,
            10,
            8,
            "latestValue",
            Some(1),
            None,
            "data",
        ),
        (
            "latest-empty-key",
            2,
            9,
            10,
            8,
            "latestValue",
            Some(0),
            None,
            "invalid_response",
        ),
        (
            "latest-impossible-durable",
            2,
            9,
            10,
            11,
            "latestValue",
            Some(1),
            None,
            "invalid_response",
        ),
        (
            "all-held-flush",
            1,
            1,
            4,
            0,
            "record",
            None,
            Some(32),
            "data",
        ),
        (
            "all-short-commit",
            1,
            1,
            4,
            0,
            "record",
            None,
            Some(31),
            "invalid_response",
        ),
    ] {
        let input = std::fs::read(root.join(name)).unwrap();
        let hex: String = input.iter().map(|byte| format!("{byte:02x}")).collect();
        let result =
            real.call(json!({"action":"decode", "hex":hex, "delivery":delivery, "from":"0"}));
        assert_eq!(result["after"], after.to_string(), "{name}");
        assert_eq!(result["accepted"], accepted.to_string(), "{name}");
        assert_eq!(result["durable"], durable.to_string(), "{name}");
        assert_eq!(result["item"], item, "{name}");
        assert_eq!(result["key"], json!(key), "{name}");
        assert_eq!(result["commit"], json!(commit), "{name}");
        assert_eq!(result["outcome"], outcome, "{name}");
        oracle::verify(&result, delivery, 0);
    }
}

fn recovery(fault: &str) {
    for event in ["disconnect", "move"] {
        for trace in schedules::traces(
            if event == "move" {
                &["move", "release"]
            } else {
                &["disconnect", "release"]
            },
            &["consume"],
        ) {
            let mut real = driver(fault);
            real.call(json!({"action":"ready"}));
            for action in trace {
                real.call(json!({"action":action}));
            }
            let state = real.call(json!({"action":"finish"}));
            assert_eq!(
                state["failure"],
                serde_json::Value::Null,
                "LATEST recovery failed: {state}"
            );
            assert_eq!(state["settled"], true, "LATEST recovery left next pending");
            assert_eq!(
                state["seen"],
                json!(["2", "5", "9"]),
                "LATEST recovery changed delivery cuts"
            );
            assert_eq!(state["attention"], json!(["5"]));
            let state = real.call(json!({"action":"stop"}));
            assert_eq!(state["outstanding"], 0);
            assert_eq!(state["active"], 0);
        }
    }
}
#[test]
fn latest_consumption_races_reconnect_and_movement() {
    recovery("");
}
#[test]
#[should_panic(expected = "LATEST recovery")]
fn negative_control_advances_latest_by_one_instead_of_its_cursor() {
    recovery("progress");
}

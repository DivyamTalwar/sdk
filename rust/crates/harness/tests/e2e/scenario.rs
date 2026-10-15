use acyclic_harness::test_support::{ScriptedReconciliation, ScriptedTurn};
use acyclic_harness::{
    context::{CompactionPolicy, ModelContextCapacity},
    model::ModelMessage,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    #[serde(default)]
    pub seed: BTreeMap<String, String>,
    pub prompt: String,
    pub turns: Vec<ScriptedTurn>,
    #[serde(default)]
    pub context: Vec<ModelMessage>,
    pub capacity: Option<ModelContextCapacity>,
    pub compaction: Option<CompactionPolicy>,
    pub max_output_tokens: Option<u32>,
    pub message_tokens: Option<u32>,
    #[serde(default)]
    pub recoveries: Vec<ScriptedReconciliation>,
    #[serde(default)]
    pub resume: Vec<String>,
    #[serde(default)]
    pub limits: BTreeMap<String, Value>,
    #[serde(default = "default_steps")]
    pub max_steps: u32,
    #[serde(default)]
    pub replay: bool,
    pub expect: Expectations,
}

fn default_steps() -> u32 {
    16
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectations {
    pub text: Option<String>,
    pub error: Option<String>,
    pub result: Option<Value>,
    pub request_count: Option<usize>,
    pub reconciliation_count: Option<usize>,
    #[serde(default)]
    pub events: Vec<Value>,
    #[serde(default)]
    pub not_events: Vec<Value>,
    #[serde(default)]
    pub requests: Vec<Value>,
    #[serde(default)]
    pub files: BTreeMap<String, Option<String>>,
}

impl Scenario {
    pub fn execution_limits(&self) -> Result<acyclic_harness::conversation::Limits, String> {
        let defaults = acyclic_harness::conversation::Limits::default();
        if self.limits.is_empty() {
            return Ok(defaults);
        }
        let mut value = serde_json::to_value(defaults).map_err(|e| e.to_string())?;
        for (key, override_value) in &self.limits {
            let slot = value
                .get_mut(key)
                .ok_or_else(|| format!("unknown limit: {key}"))?;
            *slot = override_value.clone();
        }
        serde_json::from_value(value).map_err(|e| e.to_string())
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let scenario: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if scenario.expect.text.is_some() == scenario.expect.error.is_some() {
            return Err("expect exactly one of text or error".into());
        }
        if scenario.expect.error.as_ref().is_some_and(String::is_empty) {
            return Err("error expectation must not be empty".into());
        }
        if scenario.resume.iter().any(String::is_empty) {
            return Err("resume error expectation must not be empty".into());
        }
        if scenario.expect.error.is_some() && (scenario.expect.result.is_some() || scenario.replay)
        {
            return Err("result and replay require a successful turn".into());
        }
        Ok(scenario)
    }
}

#[test]
fn scenarios_reject_typos_and_vacuous_results() {
    for input in [
        r#"{"prompt":"x","turns":[],"expect":{}}"#,
        r#"{"prompt":"x","turns":[],"expect":{"text":"x","error":"x"}}"#,
        r#"{"prompt":"x","turns":[],"expect":{"text":"x","typo":1}}"#,
        r#"{"prompt":"x","turns":[],"typo":1,"expect":{"text":"x"}}"#,
        r#"{"prompt":"x","turns":[],"expect":{"error":""}}"#,
        r#"{"prompt":"x","turns":[],"replay":true,"expect":{"error":"x"}}"#,
        r#"{"prompt":"x","turns":[],"expect":{"error":"x","result":{}}}"#,
        r#"{"prompt":"x","turns":[],"resume":[""],"expect":{"text":"x"}}"#,
        r#"{"prompt":"x","turns":[],"capacity":{"context_tokens":60,"output_tokens":10,"typo":1},"expect":{"text":"x"}}"#,
        r#"{"prompt":"x","turns":[],"compaction":{"kind":"typo"},"expect":{"text":"x"}}"#,
        r#"{"prompt":"x","turns":[],"message_tokens":-1,"expect":{"text":"x"}}"#,
    ] {
        assert!(
            Scenario::parse(input.as_bytes()).is_err(),
            "accepted {input}"
        );
    }
}

#[test]
fn limit_overrides_preserve_defaults_and_reject_unknown_keys() -> Result<(), String> {
    let case = Scenario::parse(
        br#"{"prompt":"x","turns":[],"limits":{"model_events_per_step":2},"expect":{"text":"x"}}"#,
    )?;
    let limits = case.execution_limits()?;
    assert_eq!(limits.model_events_per_step, 2);
    assert_eq!(
        limits.file_bytes,
        acyclic_harness::conversation::Limits::default().file_bytes
    );
    let typo =
        Scenario::parse(br#"{"prompt":"x","turns":[],"limits":{"typo":2},"expect":{"text":"x"}}"#)?;
    assert!(typo.execution_limits().is_err());
    Ok(())
}

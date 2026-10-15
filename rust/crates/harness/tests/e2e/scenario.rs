use acyclic_harness::test_support::{ScriptedReconciliation, ScriptedTurn};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    #[serde(default)]
    pub seed: BTreeMap<String, String>,
    pub prompt: String,
    pub turns: Vec<ScriptedTurn>,
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
    pub events: Option<Vec<Value>>,
    #[serde(default)]
    pub not_events: Vec<Value>,
    pub requests: Option<Vec<Value>>,
    #[serde(default)]
    pub files: BTreeMap<String, Option<String>>,
}

impl Scenario {
    pub fn load(root: &Path, path: &Path) -> Result<Self, String> {
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        let mut merged = Value::Object(serde_json::Map::new());
        let mut directory = root.to_path_buf();
        let parents = relative.parent().ok_or("scenario has no parent")?;
        for component in std::iter::once(None).chain(parents.components().map(Some)) {
            if let Some(component) = component {
                directory.push(component);
            }
            let defaults = directory.join("_defaults.json");
            match std::fs::read(&defaults) {
                Ok(bytes) => merge(
                    &mut merged,
                    serde_json::from_slice(&bytes)
                        .map_err(|e| format!("{}: {e}", defaults.display()))?,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", defaults.display())),
            }
        }
        merge(
            &mut merged,
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
        Self::from_value(merged)
    }

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
        Self::from_value(serde_json::from_slice(bytes).map_err(|e| e.to_string())?)
    }

    fn from_value(value: Value) -> Result<Self, String> {
        let scenario: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
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

fn merge(base: &mut Value, delta: Value) {
    match (base, delta) {
        (Value::Object(base), Value::Object(delta)) => {
            for (key, value) in delta {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, delta) => *base = delta,
    }
}

#[test]
fn recovery_defaults_preserve_nested_overrides_arrays_and_nulls() -> super::TestResult {
    let temp = tempfile::tempdir()?;
    let family = temp.path().join("family");
    std::fs::create_dir(&family)?;
    std::fs::write(
        temp.path().join("_defaults.json"),
        r#"{
        "prompt":"root", "turns":["root"], "seed":{"a":"root","b":"kept"},
        "expect":{"text":"root","request_count":1}
    }"#,
    )?;
    std::fs::write(
        family.join("_defaults.json"),
        r#"{
        "prompt":"family", "seed":{"a":"family"}, "expect":{"text":"family"}
    }"#,
    )?;
    let path = family.join("case.json");
    std::fs::write(
        &path,
        r#"{"turns":[],"expect":{"text":"case","request_count":null}}"#,
    )?;
    let case = Scenario::load(temp.path(), &path)?;
    assert_eq!(case.prompt, "family");
    assert!(case.turns.is_empty());
    assert_eq!(case.expect.request_count, None);
    assert_eq!(case.expect.text.as_deref(), Some("case"));
    assert_eq!(case.seed.get("a").map(String::as_str), Some("family"));
    assert_eq!(case.seed.get("b").map(String::as_str), Some("kept"));
    std::fs::write(&path, r#"{"typo":1}"#)?;
    assert!(Scenario::load(temp.path(), &path).is_err());
    std::fs::write(family.join("_defaults.json"), "{")?;
    assert!(Scenario::load(temp.path(), &path).is_err());
    Ok(())
}

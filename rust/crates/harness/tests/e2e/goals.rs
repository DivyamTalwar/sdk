use acyclic_harness::{goal::GoalContinuationPolicy, test_support::ScriptedTurn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GoalCase {
    #[serde(default)]
    pub seed: BTreeMap<String, String>,
    pub prompt: String,
    pub turns: Vec<ScriptedTurn>,
    #[serde(default = "one_step")]
    pub max_steps: u32,
    #[serde(default)]
    pub settles: u32,
    #[serde(default)]
    pub policy: GoalContinuationPolicy,
    pub expect: Expectations,
}

fn one_step() -> u32 {
    1
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Expectations {
    pub text: String,
    pub request_prompt: String,
    #[serde(default)]
    pub unchanged_goal: bool,
    pub result: Value,
    pub view: Option<Value>,
    pub error: Option<String>,
    pub files: BTreeMap<String, Option<String>>,
    pub requests: Option<Vec<Value>>,
}

impl GoalCase {
    pub(super) fn parse(value: Value) -> Result<Self, String> {
        let case: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if case.expect.view.is_some() == case.expect.error.is_some() {
            return Err("expect exactly one of goal view or error".into());
        }
        if case.expect.error.as_ref().is_some_and(String::is_empty) {
            return Err("goal error must not be empty".into());
        }
        for value in std::iter::once(&case.expect.result).chain(case.expect.view.as_ref()) {
            if value.as_object().is_none_or(|object| object.is_empty()) {
                return Err("goal assertions must be nonempty objects".into());
            }
        }
        if !case.expect.files.contains_key("GOAL.md") {
            return Err("assert final GOAL.md bytes or absence".into());
        }
        if case.expect.unchanged_goal && !case.seed.contains_key("GOAL.md") {
            return Err("unchanged goal assertion requires a GOAL.md seed".into());
        }
        Ok(case)
    }
}

#[cfg(test)]
#[path = "goals_tests.rs"]
mod tests;

//! Configured command hooks shared by native harness sessions.

use crate::{Error, Result};
use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

/// Lifecycle points with command-hook support.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HookEvent {
    /// A session was opened, resumed, forked, or compacted.
    SessionStart,
    /// A tool is about to execute.
    PreToolUse,
    /// A tool returned successfully.
    PostToolUse,
    /// The model is about to finish its turn.
    Stop,
}

impl HookEvent {
    fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
        }
    }
}

#[derive(Deserialize)]
struct CommandHook {
    #[serde(rename = "type")]
    kind: String,
    command: String,
    #[serde(default = "default_timeout")]
    timeout: u64,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default, rename = "async")]
    asynchronous: bool,
}

fn default_timeout() -> u64 {
    60
}

#[derive(Deserialize)]
struct Group {
    #[serde(default)]
    matcher: String,
    hooks: Vec<CommandHook>,
}

struct Binding {
    event: HookEvent,
    matcher: Option<Regex>,
    commands: Vec<Arc<CommandHook>>,
}

#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    hooks: BTreeMap<String, Value>,
}

/// Validated hooks, with matchers compiled once during configuration loading.
#[derive(Default)]
pub struct Hooks {
    bindings: Vec<Binding>,
}

impl Hooks {
    /// Adds a Claude settings file or a Codex hooks.json document atomically.
    pub fn add_json(&mut self, document: &[u8]) -> Result<()> {
        let document = serde_json::from_slice(document)
            .map_err(|error| Error::Invalid(format!("hook configuration: {error}")))?;
        self.add_document(document)
    }

    /// Adds the inline hooks table from a Codex config.toml document.
    pub fn add_toml(&mut self, document: &str) -> Result<()> {
        let document: Document = toml::from_str(document)
            .map_err(|error| Error::Invalid(format!("hook configuration: {error}")))?;
        self.add_document(document)
    }

    fn add_document(&mut self, document: Document) -> Result<()> {
        let mut bindings = Vec::new();
        for (name, groups) in document.hooks {
            let event = match name.as_str() {
                "SessionStart" => HookEvent::SessionStart,
                "PreToolUse" => HookEvent::PreToolUse,
                "PostToolUse" => HookEvent::PostToolUse,
                "Stop" => HookEvent::Stop,
                _ => continue,
            };
            let groups: Vec<Group> = serde_json::from_value(groups)
                .map_err(|error| Error::Invalid(format!("hook configuration: {error}")))?;
            for group in groups {
                let matcher = if group.matcher.is_empty()
                    || group.matcher == "*"
                    || event == HookEvent::Stop
                {
                    None
                } else {
                    Some(
                        Regex::new(&group.matcher)
                            .map_err(|error| Error::Invalid(format!("hook matcher: {error}")))?,
                    )
                };
                for hook in &group.hooks {
                    if hook.kind != "command" || hook.asynchronous {
                        return Err(Error::Unsupported(
                            "only command hooks are supported".into(),
                        ));
                    }
                    if hook.shell.as_deref().is_some_and(|shell| {
                        !matches!(shell, "bash" | "sh" | "powershell" | "pwsh")
                    }) {
                        return Err(Error::Invalid("unknown hook shell".into()));
                    }
                    if hook.command.trim().is_empty()
                        || hook.command.contains('\0')
                        || hook.timeout == 0
                    {
                        return Err(Error::Invalid(
                            "hook command and timeout must be positive".into(),
                        ));
                    }
                }
                bindings.push(Binding {
                    event,
                    matcher,
                    commands: group.hooks.into_iter().map(Arc::new).collect(),
                });
            }
        }
        self.bindings.extend(bindings);
        Ok(())
    }

    fn commands<'a>(
        &'a self,
        event: HookEvent,
        input: &'a Value,
    ) -> impl Iterator<Item = &'a Arc<CommandHook>> {
        let subject = match event {
            HookEvent::SessionStart => input.get("source"),
            HookEvent::PreToolUse | HookEvent::PostToolUse => input.get("tool_name"),
            HookEvent::Stop => None,
        }
        .and_then(Value::as_str)
        .unwrap_or("");
        let alias = match subject {
            "bash" => Some("Bash"),
            "read" => Some("Read"),
            "write" => Some("Write"),
            "edit" => Some("Edit"),
            _ => None,
        };
        self.bindings
            .iter()
            .filter(move |binding| {
                binding.event == event
                    && binding.matcher.as_ref().is_none_or(|matcher| {
                        matcher.is_match(subject)
                            || alias.is_some_and(|alias| matcher.is_match(alias))
                    })
            })
            .flat_map(|binding| &binding.commands)
    }
}

/// Applied hook decisions and context for the harness loop.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct HookOutput {
    /// Reason a tool or stop was blocked.
    pub blocked: Option<String>,
    /// Extra context to include in the next model request.
    pub additional_context: Vec<String>,
    /// Replacement arguments returned by a pre-tool hook.
    pub updated_input: Option<Value>,
}

impl HookOutput {
    fn observe(
        &mut self,
        event: HookEvent,
        code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) -> Result<()> {
        if code == Some(2) {
            if matches!(event, HookEvent::PreToolUse | HookEvent::Stop) {
                self.blocked = Some(String::from_utf8_lossy(stderr).into_owned());
            } else if event == HookEvent::PostToolUse {
                self.additional_context
                    .push(String::from_utf8_lossy(stderr).into_owned());
            }
            return Ok(());
        }
        if code != Some(0) {
            return Ok(());
        }
        let text = std::str::from_utf8(stdout)
            .map_err(|error| Error::Invalid(error.to_string()))?
            .trim();
        if text.is_empty() {
            return Ok(());
        }
        let Ok(mut value) = serde_json::from_str::<Value>(text) else {
            if event == HookEvent::SessionStart {
                self.additional_context.push(text.into());
            }
            return Ok(());
        };
        let mut specific = value
            .get_mut("hookSpecificOutput")
            .map(Value::take)
            .unwrap_or(Value::Null);
        if specific
            .get("hookEventName")
            .and_then(Value::as_str)
            .is_some_and(|name| name != event.name())
        {
            return Err(Error::Invalid("hook output names another event".into()));
        }
        if value.get("continue").and_then(Value::as_bool) == Some(false) {
            self.blocked = Some(
                take_string(&mut value, "stopReason").unwrap_or_else(|| "Stopped by hook".into()),
            );
        }
        if matches!(event, HookEvent::PreToolUse | HookEvent::Stop)
            && (value.get("decision").and_then(Value::as_str) == Some("block")
                || specific.get("permissionDecision").and_then(Value::as_str) == Some("deny"))
        {
            self.blocked = Some(
                take_string(&mut specific, "permissionDecisionReason")
                    .or_else(|| take_string(&mut value, "reason"))
                    .unwrap_or_else(|| "Blocked by hook".into()),
            );
        }
        if let Some(context) = take_string(&mut specific, "additionalContext") {
            self.additional_context.push(context);
        }
        if event == HookEvent::PreToolUse
            && let Some(input) = specific
                .as_object_mut()
                .and_then(|fields| fields.remove("updatedInput"))
        {
            if !input.is_object() {
                return Err(Error::Invalid("hook updatedInput must be an object".into()));
            }
            self.updated_input = Some(input);
        }
        Ok(())
    }
}

fn take_string(value: &mut Value, name: &str) -> Option<String> {
    let Value::String(text) = value.as_object_mut()?.remove(name)? else {
        return None;
    };
    Some(text)
}

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(test)]
mod tests;

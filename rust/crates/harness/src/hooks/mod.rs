//! Configured command hooks shared by native harness sessions.

use crate::{Error, Result, tool::ToolRegistry};
use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, sync::Arc};

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
    #[serde(default)]
    args: Option<Vec<String>>,
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
    matcher: Option<Matcher>,
    commands: Vec<Arc<CommandHook>>,
}

enum Matcher {
    Exact(String),
    Pattern(Regex),
}

impl Matcher {
    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(expected) => expected == name,
            Self::Pattern(pattern) => pattern.is_match(name),
        }
    }
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
                let matcher =
                    if group.matcher.is_empty() || group.matcher == "*" || event == HookEvent::Stop
                    {
                        None
                    } else if group.matcher.chars().any(|character| {
                        matches!(
                            character,
                            '.' | '^'
                                | '$'
                                | '*'
                                | '+'
                                | '?'
                                | '('
                                | ')'
                                | '['
                                | ']'
                                | '{'
                                | '}'
                                | '|'
                                | '\\'
                        )
                    }) {
                        Some(Matcher::Pattern(Regex::new(&group.matcher).map_err(
                            |error| Error::Invalid(format!("hook matcher: {error}")),
                        )?))
                    } else {
                        Some(Matcher::Exact(group.matcher))
                    };
                for hook in &group.hooks {
                    if hook.kind != "command" || hook.asynchronous {
                        return Err(Error::Unsupported(
                            "only command hooks are supported".into(),
                        ));
                    }
                    if hook.shell.as_deref().is_some_and(|shell| {
                        !matches!(shell, "bash" | "sh" | "powershell" | "pwsh" | "cmd")
                    }) {
                        return Err(Error::Invalid("unknown hook shell".into()));
                    }
                    if hook.command.trim().is_empty()
                        || hook.command.contains('\0')
                        || hook
                            .args
                            .as_ref()
                            .is_some_and(|args| args.iter().any(|arg| arg.contains('\0')))
                        || (hook.args.is_some() && hook.shell.is_some())
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
        registry: &'a ToolRegistry,
    ) -> Result<impl Iterator<Item = &'a Arc<CommandHook>>> {
        let subject = match event {
            HookEvent::SessionStart => input.get("source"),
            HookEvent::PreToolUse | HookEvent::PostToolUse => input.get("tool_name"),
            HookEvent::Stop => None,
        }
        .and_then(Value::as_str)
        .unwrap_or("");
        let tool = if matches!(event, HookEvent::PreToolUse | HookEvent::PostToolUse) {
            registry.get(subject, None, None)?
        } else {
            None
        };
        Ok(self
            .bindings
            .iter()
            .filter(move |binding| {
                binding.event == event
                    && binding.matcher.as_ref().is_none_or(|matcher| {
                        matcher.matches(subject)
                            || tool.is_some_and(|tool| {
                                matcher.matches(&tool.definition.name)
                                    || tool.hook_names.iter().any(|name| matcher.matches(name))
                            })
                    })
            })
            .flat_map(|binding| &binding.commands))
    }

    async fn run_commands<F, Fut>(
        &self,
        event: HookEvent,
        input: &Value,
        registry: &ToolRegistry,
        execute: F,
    ) -> Result<HookOutput>
    where
        F: FnMut(&Arc<CommandHook>) -> Fut,
        Fut: Future<Output = Result<(Option<i32>, Vec<u8>, Vec<u8>)>>,
    {
        let results =
            futures::future::join_all(self.commands(event, input, registry)?.map(execute)).await;
        let mut output = HookOutput::default();
        for result in results {
            let observed = match result {
                Ok((code, stdout, stderr)) => output.observe(event, code, &stdout, &stderr),
                Err(error) => Err(error),
            };
            if let Err(error) = observed {
                output
                    .diagnostics
                    .push(format!("{} hook failed: {error}", event.name()));
            }
        }
        Ok(output)
    }
}

/// Applied hook decisions and context for the harness loop.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct HookOutput {
    /// Reason a tool or stop was blocked.
    pub blocked: Option<String>,
    /// A pre-tool hook requested human approval, with its explanation.
    pub approval: Option<String>,
    /// Extra context to include in the next model request.
    pub additional_context: Vec<String>,
    /// Replacement arguments returned by a pre-tool hook.
    pub updated_input: Option<Value>,
    /// Non-blocking command failures, for the caller's session diagnostics.
    pub diagnostics: Vec<String>,
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
                self.blocked
                    .get_or_insert_with(|| String::from_utf8_lossy(stderr).into_owned());
            } else if event == HookEvent::PostToolUse {
                self.additional_context
                    .push(String::from_utf8_lossy(stderr).into_owned());
            } else {
                self.failure(event, code, stderr);
            }
            return Ok(());
        }
        if code != Some(0) {
            self.failure(event, code, stderr);
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
            self.blocked.get_or_insert_with(|| {
                take_string(&mut value, "stopReason").unwrap_or_else(|| "Stopped by hook".into())
            });
        }
        if matches!(event, HookEvent::PreToolUse | HookEvent::Stop)
            && (value.get("decision").and_then(Value::as_str) == Some("block")
                || specific.get("permissionDecision").and_then(Value::as_str) == Some("deny"))
        {
            self.blocked.get_or_insert_with(|| {
                take_string(&mut specific, "permissionDecisionReason")
                    .or_else(|| take_string(&mut value, "reason"))
                    .unwrap_or_else(|| "Blocked by hook".into())
            });
        }
        if event == HookEvent::PreToolUse
            && specific.get("permissionDecision").and_then(Value::as_str) == Some("ask")
        {
            self.approval.get_or_insert_with(|| {
                take_string(&mut specific, "permissionDecisionReason")
                    .unwrap_or_else(|| "Approval requested by hook".into())
            });
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

    fn failure(&mut self, event: HookEvent, code: Option<i32>, stderr: &[u8]) {
        self.diagnostics.push(format!(
            "{} hook failed (exit {code:?}): {}",
            event.name(),
            String::from_utf8_lossy(stderr)
        ));
    }
}

fn take_string(value: &mut Value, name: &str) -> Option<String> {
    let Value::String(text) = value.as_object_mut()?.remove(name)? else {
        return None;
    };
    Some(text)
}

mod native;
pub use native::HooksConfig;
#[cfg(test)]
mod tests;

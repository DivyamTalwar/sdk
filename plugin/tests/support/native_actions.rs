//! Native processes execute commands; assertions read their published SDK files.
use super::native_hooks::{self, Result};
use acyclic_harness::{
    TaskId,
    external_executor::{ExternalHost, request},
    task_volume::{Component, Query, TaskVolume},
};
use acyclic_stream::grpc::Client;
use serde_json::{Value, json};
use std::path::Path;

pub struct Tools<'a> {
    cwd: &'a Path,
    host: ExternalHost,
    session: String,
    caller: Option<String>,
    pub task: TaskId,
    results: TaskVolume<Client>,
}

impl<'a> Tools<'a> {
    pub async fn open(cwd: &'a Path, host: ExternalHost) -> Result<Self> {
        let session = uuid::Uuid::new_v4().to_string();
        let event = if host == ExternalHost::Pi {
            "session_start"
        } else {
            "SessionStart"
        };
        let input = json!({"session_id":session,"cwd":cwd});
        let task = request(host, event, input.clone())?.task_id();
        native_hooks::hook(cwd, host.name(), event, input).await?;
        let results = native_hooks::files(cwd, &format!("external/results/{task}"))
            .await
            .map_err(|error| format!("{} attach results: {error}", host.name()))?;
        Ok(Self {
            cwd,
            host,
            session,
            caller: None,
            task,
            results,
        })
    }

    pub async fn child(&self, caller: &str) -> Result<Self> {
        let event = if self.host == ExternalHost::Pi {
            "subagent_start"
        } else {
            "SubagentStart"
        };
        let input = json!({"session_id":self.session,"agent_id":caller,
            "parent_agent_id":self.caller,"cwd":self.cwd});
        native_hooks::hook(self.cwd, self.host.name(), event, input).await?;
        let event = if self.host == ExternalHost::Pi {
            "session_start"
        } else {
            "SessionStart"
        };
        let input = json!({"session_id":self.session,"agent_id":caller,"cwd":self.cwd});
        let task = request(self.host, event, input.clone())?.task_id();
        native_hooks::hook(self.cwd, self.host.name(), event, input).await?;
        Ok(Self {
            cwd: self.cwd,
            host: self.host,
            session: self.session.clone(),
            caller: Some(caller.to_owned()),
            task,
            results: native_hooks::files(self.cwd, &format!("external/results/{task}"))
                .await
                .map_err(|error| format!("{} child {caller} results: {error}", self.host.name()))?,
        })
    }

    async fn outcome(&self, id: &str, name: &str, arguments: Value) -> Result<(Value, bool)> {
        let event = if self.host == ExternalHost::Pi {
            "tool_call"
        } else {
            "PreToolUse"
        };
        let mut input = json!({"session_id":self.session,"agent_id":self.caller,"tool_use_id":id,
            "tool_name":name,"tool_input":arguments});
        let call = request(self.host, event, input.clone())?;
        if self.host != ExternalHost::Pi {
            input["tool_name"] = json!(format!("mcp__acyclic__{name}"));
        }
        let child = native_hooks::start_hook(self.cwd, self.host.name(), event, input).await?;
        let output = child.wait_with_output().await?;
        let key = call.result_key(Component::new(self.task.to_string())?)?;
        let snapshot = self
            .results
            .snapshot(&Query::File(key), 1)
            .await
            .map_err(|error| format!("{} {name} {id} result query: {error}", self.host.name()))?;
        assert_eq!(
            snapshot.files.len(),
            1,
            "{} {name}: {}",
            self.host.name(),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok((
            serde_json::from_slice(&snapshot.files[0].bytes)?,
            output.status.success(),
        ))
    }

    pub async fn call(&self, id: &str, name: &str, arguments: Value) -> Result<Value> {
        let (outcome, success) = self.outcome(id, name, arguments).await?;
        assert!(success, "{} {name}: {outcome}", self.host.name());
        outcome
            .get("Succeeded")
            .cloned()
            .ok_or_else(|| format!("{name}: {outcome}").into())
    }

    pub async fn reject(&self, id: &str, name: &str, arguments: Value) -> Result<Value> {
        let (outcome, success) = self.outcome(id, name, arguments).await?;
        assert!(!success, "{} {name}: {outcome}", self.host.name());
        assert!(outcome.get("Failed").is_some(), "{outcome}");
        Ok(outcome)
    }
}

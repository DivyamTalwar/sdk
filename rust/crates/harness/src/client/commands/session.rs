//! Command composition over the existing volume snapshot and push follow.
use super::*;
use crate::{OperationId, TaskId, task_volume as files};
use acyclic_stream::StreamProvider;
use futures::StreamExt as _;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};

/// Disposable command/UI fold. All authoritative contents and execution markers
/// are ordinary files in the supplied, already-authorized private/config volume.
pub struct CommandSession<P: ?Sized> {
    volume: Arc<files::TaskVolume<P>>,
    changes: files::ChangeStream,
    generation: files::Generation,
    files: BTreeMap<files::FileKey, files::FileValue>,
    pending: VecDeque<files::FileValue>,
    catalog: CommandCatalog,
    session: TaskId,
    maximum_bytes: u64,
    maximum_files: usize,
}

fn storage(error: files::Error) -> Error { Error::Storage(error.to_string()) }
fn component(text: impl AsRef<str>) -> Result<files::Component> {
    files::Component::new(text).map_err(storage)
}
fn ui(directory: &str, name: &str) -> Result<files::FileKey> {
    Ok(files::FileKey::Ui { directory: component(directory)?, name: component(name)? })
}

impl<P: StreamProvider + ?Sized> CommandSession<P> {
    /// Open one atomic snapshot and its exact cursor follow. Reopening discovers
    /// unconsumed requests and never reruns a claimed external effect.
    pub async fn open(volume: Arc<files::TaskVolume<P>>, session: TaskId, maximum_bytes: u64, maximum_files: usize) -> Result<Self> {
        if maximum_bytes == 0 || maximum_files == 0 { return Err(Error::Invalid("zero command-session bound".into())); }
        let observed = volume.observe(files::Query::Root, None, maximum_files).await.map_err(storage)?;
        let snapshot = observed.snapshot.ok_or_else(|| Error::Conflict("command snapshot missing".into()))?;
        let mut client = Self { volume, changes: observed.changes, generation: snapshot.generation,
            files: BTreeMap::new(), pending: VecDeque::new(), catalog: CommandCatalog::builtins()?,
            session, maximum_bytes, maximum_files };
        for file in snapshot.files { client.fold(file)?; }
        for (key, bytes) in [
            (ui("config", "commands.json")?, DEFAULT_COMMANDS_JSON.to_vec()),
            (ui("config", "settings.json")?, serde_json::to_vec(&json!({"diff_mode":"auto","diff_modes":["auto","unified","split"]})).map_err(codec)?),
            (client.state_key(CommandFileRoot::Session, "layout.json")?, serde_json::to_vec(&json!({"selected_task":session})).map_err(codec)?),
        ] {
            if !client.files.contains_key(&key) { client.publish(key, bytes, true).await?; }
        }
        Ok(client)
    }

    /// Catalog from the latest observed command-definition file.
    pub fn catalog(&self) -> &CommandCatalog { &self.catalog }

    /// The selected task is persisted in the session layout, not the input loop.
    pub fn selected_task(&self) -> Result<TaskId> {
        let key = ui(&format!("session-{}", self.session), "layout.json")?;
        let value: Value = self.files.get(&key).map(|file| serde_json::from_slice(&file.bytes)).transpose().map_err(codec)?.unwrap_or(Value::Null);
        value.get("selected_task").and_then(Value::as_str).map(TaskId::parse)
            .unwrap_or(Ok(self.session))
    }

    fn state_key(&self, root: CommandFileRoot, name: &str) -> Result<files::FileKey> {
        let directory = match root {
            CommandFileRoot::Config => "config".into(),
            CommandFileRoot::Session => format!("session-{}", self.session),
            CommandFileRoot::Task => format!("task-{}", self.selected_task()?),
        };
        ui(&directory, name)
    }

    fn snapshot(&self) -> Result<CommandFileSnapshot> {
        let mut snapshot = BTreeMap::new();
        for root in [CommandFileRoot::Config, CommandFileRoot::Session, CommandFileRoot::Task] {
            let mut values = BTreeMap::new();
            for name in ["settings.json", "layout.json", "theme.json", "attention.json"] {
                if let Some(file) = self.files.get(&self.state_key(root, name)?) {
                    values.insert(name.into(), serde_json::from_slice(&file.bytes).map_err(codec)?);
                }
            }
            snapshot.insert(root, values);
        }
        Ok(snapshot)
    }

    /// Resolve at the current file-query cut, then publish the ordinary file
    /// change with that generation as its precondition. Stale cuts fail closed.
    pub async fn submit(&mut self, input: &str, reader: &dyn ContentResidencyVerifier) -> Result<OperationId> {
        let resolution = self.catalog.resolve(input, reader, self.maximum_bytes).await?;
        let operation = OperationId::new();
        for change in resolution.file_changes(&self.snapshot()?, operation)? {
            let key = if change.create_only {
                let task = if change.root == CommandFileRoot::Session { self.session } else { self.selected_task()? };
                files::FileKey::Task { task: component(task.to_string())?, file: files::TaskFile::Request(component(operation.to_string())?) }
            } else { self.state_key(change.root, &change.path)? };
            self.publish(key, serde_json::to_vec(&change.contents).map_err(codec)?, change.create_only).await?;
        }
        Ok(operation)
    }

    fn fold(&mut self, file: files::FileValue) -> Result<()> {
        if file.bytes.len() as u64 > self.maximum_bytes { return Err(Error::Invalid("command file exceeds byte bound".into())); }
        if !self.files.contains_key(&file.key) && self.files.len() >= self.maximum_files {
            return Err(Error::Invalid("command query exceeds file bound".into()));
        }
        if file.key == ui("config", "commands.json")? {
            self.catalog = CommandCatalog::from_bytes(&file.bytes, self.maximum_bytes)?;
        }
        if matches!(file.key, files::FileKey::Task { file: files::TaskFile::Request(_), .. }) && !self.files.contains_key(&file.key) {
            self.pending.push_back(file.clone());
        }
        self.files.insert(file.key.clone(), file);
        Ok(())
    }

    async fn advance(&mut self) -> Result<()> {
        let change = self.changes.next().await.ok_or_else(|| Error::Conflict("command follow ended; reopen snapshot".into()))?.map_err(storage)?;
        if let Some(file) = change.file { self.fold(file)?; }
        self.generation = files::Generation { volume: change.cursor.volume, epoch: change.cursor.epoch,
            position: change.cursor.position, commit: change.cursor.commit };
        Ok(())
    }

    async fn publish(&mut self, key: files::FileKey, bytes: Vec<u8>, create_only: bool) -> Result<()> {
        if bytes.len() as u64 > self.maximum_bytes { return Err(Error::Invalid("command publication exceeds byte bound".into())); }
        if create_only && self.files.contains_key(&key) { return Err(Error::Conflict("command file already exists".into())); }
        let generation = self.volume.append(self.generation, &files::FileValue::new(key, bytes)).await.map_err(storage)?;
        while self.generation.position < generation.position { self.advance().await?; }
        Ok(())
    }

    /// Wait on the existing push follow; no timer, watcher or polling loop.
    /// Non-request file changes refresh the fold before the next pending request.
    pub async fn next_request(&mut self) -> Result<files::FileValue> {
        loop {
            if let Some(request) = self.pending.pop_front() { return Ok(request); }
            self.advance().await?;
        }
    }

    /// Claim an immutable request before dispatch. A second process must win the
    /// same generation CAS and immutable claim key; it cannot execute a duplicate.
    /// A crash after claiming leaves an explicit uncertain effect, never a resend.
    pub async fn consume<F, Fut>(&mut self, request: &files::FileValue, execute: F) -> Result<Option<Value>>
    where F: FnOnce(OperationId, TaskId, CommandResolution) -> Fut,
          Fut: std::future::Future<Output = Result<Value>> {
        let files::FileKey::Task { task, file: files::TaskFile::Request(id) } = &request.key else {
            return Err(Error::Invalid("expected a command request file".into()));
        };
        let operation = OperationId::parse(id.as_str())?;
        let task_id = TaskId::parse(task.as_str())?;
        let key = |file| files::FileKey::Task { task: task.clone(), file: files::TaskFile::Tool { turn: 0, call: id.clone(), file } };
        let claim = key(files::ToolFile::Request);
        if self.files.contains_key(&claim) { return Ok(None); }
        let resolution: CommandResolution = serde_json::from_slice(&request.bytes).or_else(|_| {
            serde_json::from_slice(&request.bytes).map(|request| CommandResolution::Submit { request })
        }).map_err(codec)?;
        let bytes = serde_json::to_vec(&json!({"request":resolution,"attempt":OperationId::new()})).map_err(codec)?;
        self.publish(claim, bytes, true).await?;
        let result = execute(operation, task_id, resolution).await;
        let value = match &result { Ok(value) => json!({"ok":value}), Err(error) => json!({"error":error.to_string()}) };
        self.publish(key(files::ToolFile::Result), serde_json::to_vec(&value).map_err(codec)?, true).await?;
        result.map(Some)
    }

    /// Query-derived exit flag used by native and renderer clients alike.
    pub fn exit_requested(&self) -> Result<bool> {
        let key = self.state_key(CommandFileRoot::Session, "layout.json")?;
        let value: Value = self.files.get(&key).map(|file| serde_json::from_slice(&file.bytes)).transpose().map_err(codec)?.unwrap_or(Value::Null);
        Ok(value.get("exit_requested").and_then(Value::as_bool).unwrap_or(false))
    }
}

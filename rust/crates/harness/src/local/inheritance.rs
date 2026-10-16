//! Admission-time inheritance and disposable, file-backed ancestry memoization.
use super::*;
use crate::{
    runtime::TaskStateProvider,
    task_volume::{FileKey, FileValue, Query, TaskVolume},
};
use acyclic_stream::{BoxProviderFuture, StreamProvider};
use serde_json::{Value, json};
use std::collections::HashSet;

fn key(task: TaskId, name: &str) -> Result<FileKey> {
    let path = if matches!(name, "settings" | "context") {
        format!(".ui/task-{task}/{name}.json")
    } else {
        format!(".tasks/{task}/context/command-{name}/manifest.json")
    };
    FileKey::parse(&path).map_err(storage)
}
async fn read<P: StreamProvider + ?Sized>(
    volume: &TaskVolume<P>,
    key: &FileKey,
    maximum: u64,
) -> Result<Option<Value>> {
    let cut = volume
        .snapshot(&Query::File(key.clone()), 1)
        .await
        .map_err(storage)?;
    cut.files
        .first()
        .map(|file| decode(&file.bytes, maximum))
        .transpose()
}
fn decode(bytes: &[u8], maximum: u64) -> Result<Value> {
    if bytes.len() as u64 > maximum {
        return Err(Error::Invalid("inherited state exceeds file bound".into()));
    }
    serde_json::from_slice(bytes).map_err(storage)
}
fn validate(value: &Value) -> Result<()> {
    if !value.is_object()
        || ["model", "summary"]
            .iter()
            .any(|field| !value[*field].is_null() && !value[*field].is_string())
    {
        return Err(Error::Invalid(
            "invalid inherited settings or context".into(),
        ));
    }
    serde_json::from_value::<Option<TaskId>>(
        value
            .get("parent")
            .cloned()
            .ok_or_else(|| Error::Invalid("inherited parent is absent".into()))?,
    )
    .map_err(storage)?;
    Ok(())
}
// The parent marker is published last: its presence means both independently
// bounded values are retained. Each immutable component uses the same modeled
// first-publication path, including retry/reopen after a partial publication.
async fn read_inheritance<P: StreamProvider + ?Sized>(
    volume: &TaskVolume<P>,
    task: TaskId,
    maximum: u64,
) -> Result<Option<Value>> {
    let Some(parent) = read(volume, &key(task, "inherited")?, maximum).await? else {
        return Ok(None);
    };
    let mut value = json!({"parent":parent});
    for field in ["model", "summary"] {
        value[field] = read(volume, &key(task, &format!("inherited-{field}"))?, maximum)
            .await?
            .ok_or_else(|| Error::Invalid(format!("retained {field} is absent")))?;
    }
    validate(&value)?;
    Ok(Some(value))
}
async fn retain_inheritance<P: StreamProvider + ?Sized>(
    volume: &TaskVolume<P>,
    task: TaskId,
    mut value: Value,
    maximum: u64,
) -> Result<Value> {
    validate(&value)?;
    for field in ["model", "summary", "parent"] {
        let name = if field == "parent" {
            "inherited".into()
        } else {
            format!("inherited-{field}")
        };
        value[field] = retain(volume, key(task, &name)?, value[field].take(), maximum).await?;
    }
    Ok(value)
}
// First publication pins inheritance. A competing creator's accepted value,
// including after a lost acknowledgement, is always the value returned.
async fn retain<P: StreamProvider + ?Sized>(
    volume: &TaskVolume<P>,
    key: FileKey,
    value: Value,
    maximum: u64,
) -> Result<Value> {
    let cut = volume
        .snapshot(&Query::File(key.clone()), 1)
        .await
        .map_err(storage)?;
    retain_at_cut(volume, key, value, maximum, cut).await
}
async fn retain_at_cut<P: StreamProvider + ?Sized>(
    volume: &TaskVolume<P>,
    key: FileKey,
    value: Value,
    maximum: u64,
    mut cut: crate::task_volume::Snapshot,
) -> Result<Value> {
    let bytes = serde_json::to_vec(&value).map_err(storage)?;
    decode(&bytes, maximum)?;
    loop {
        if let Some(file) = cut.files.first() {
            return decode(&file.bytes, maximum);
        }
        match volume
            .create(cut.generation, &FileValue::new(key.clone(), bytes.clone()))
            .await
        {
            Ok(_) => return Ok(value),
            Err(crate::task_volume::Error::Conflict(_)) => {
                cut = volume
                    .snapshot(&Query::File(key.clone()), 1)
                    .await
                    .map_err(storage)?;
            }
            Err(error) => return Err(storage(error)),
        }
    }
}
impl LocalHarness {
    pub(super) async fn prepare_inheritance(
        &self,
        task: TaskId,
        parent: Option<TaskId>,
    ) -> Result<()> {
        if read_inheritance(&self.task_files.inner, task, self.limits.file_bytes)
            .await?
            .is_some()
        {
            return Ok(());
        }
        let mut value = match parent {
            Some(parent) => self.resolved_inheritance(parent).await?,
            None => json!({"model":null,"summary":null}),
        };
        value["parent"] = json!(parent);
        retain_inheritance(&self.task_files.inner, task, value, self.limits.file_bytes).await?;
        Ok(())
    }
    fn inherited(&self, task: TaskId) -> BoxProviderFuture<'_, Result<Value>> {
        Box::pin(async move {
            if let Some(value) =
                read_inheritance(&self.task_files.inner, task, self.limits.file_bytes).await?
            {
                return Ok(value);
            }
            // Upgrade admissions made before inheritance was retained. Stop at the
            // first saved binding and publish each missing prefix exactly once.
            let record = self.runtime.task_host().observe_admission(task).await?;
            let context = self
                .runtime
                .harness()
                .durable_context(task, record.operation_id, None)
                .await?;
            let ancestors = context.ancestors(task)?;
            futures::pin_mut!(ancestors);
            let mut missing = Vec::new();
            let mut parent = None;
            let mut value = json!({"model":null,"summary":null});
            while let Some(candidate) = futures::TryStreamExt::try_next(&mut ancestors).await? {
                if let Some(saved) =
                    read_inheritance(&self.task_files.inner, candidate, self.limits.file_bytes)
                        .await?
                {
                    value = saved;
                    parent = Some(candidate);
                    break;
                }
                missing.push(candidate);
            }
            for candidate in missing.into_iter().rev() {
                if let Some(parent) = parent {
                    value = self.apply_own_inheritance(parent, value).await?;
                }
                value["parent"] = json!(parent);
                value = retain_inheritance(
                    &self.task_files.inner,
                    candidate,
                    value,
                    self.limits.file_bytes,
                )
                .await?;
                parent = Some(candidate);
            }
            Ok(value)
        })
    }
    pub(super) async fn resolved_inheritance(&self, task: TaskId) -> Result<Value> {
        self.apply_own_inheritance(task, self.inherited(task).await?)
            .await
    }
    pub(super) async fn resolved_value(&self, task: TaskId, field: &str) -> Result<Option<String>> {
        if let Some(value) = self.own_value(task, field).await? {
            return Ok(Some(value));
        }
        let inherited = self.inherited(task).await?;
        Ok(inherited[field].as_str().map(str::to_owned))
    }
    async fn own_value(&self, task: TaskId, field: &str) -> Result<Option<String>> {
        let name = if field == "model" {
            "settings"
        } else {
            "context"
        };
        let Some(value) = read(
            &self.task_files.inner,
            &key(task, name)?,
            self.limits.file_bytes,
        )
        .await?
        else {
            return Ok(None);
        };
        if !value.is_object() {
            return Err(Error::Invalid(format!("{name} must be an object")));
        }
        match value.get(field) {
            Some(text) => text
                .as_str()
                .map(|text| Some(text.to_owned()))
                .ok_or_else(|| Error::Invalid(format!("{field} must be text"))),
            None if field == "model" => Ok(None),
            None => Err(Error::Invalid("compacted context missing summary".into())),
        }
    }
    async fn apply_own_inheritance(&self, task: TaskId, mut value: Value) -> Result<Value> {
        validate(&value)?;
        for field in ["model", "summary"] {
            if let Some(own) = self.own_value(task, field).await? {
                value[field] = json!(own);
            }
        }
        Ok(value)
    }
    pub(super) async fn command_has_ancestor(
        &self,
        task: Option<TaskId>,
        scope: TaskId,
    ) -> Result<bool> {
        let mut cursor = task;
        let mut missing = Vec::new();
        let mut visited = HashSet::new();
        let name = format!("ancestor-{scope}");
        let mut matches = false;
        while let Some(current) = cursor {
            if current == scope {
                matches = true;
                break;
            }
            if !visited.insert(current) {
                return Err(Error::Invalid("cyclic task inheritance".into()));
            }
            if let Some(saved) = read(
                &self.task_files.inner,
                &key(current, &name)?,
                self.limits.file_bytes,
            )
            .await?
            {
                matches = saved
                    .as_bool()
                    .ok_or_else(|| Error::Invalid("ancestor cache must be boolean".into()))?;
                break;
            }
            let inherited = self.inherited(current).await?;
            cursor = serde_json::from_value(inherited["parent"].clone()).map_err(storage)?;
            missing.push(current);
        }
        // The control file bounds the scope set. Each (task, scope) edge is
        // visited once; later descendants stop at their parent's cached answer.
        for task in missing {
            let saved = retain(
                &self.task_files.inner,
                key(task, &name)?,
                json!(matches),
                self.limits.file_bytes,
            )
            .await?;
            if saved != matches {
                return Err(Error::Conflict("task ancestry cache changed".into()));
            }
        }
        Ok(matches)
    }
}

#[cfg(all(test, acyclic_harness_loom))]
mod model;
#[cfg(all(test, not(acyclic_harness_loom)))]
mod tests;

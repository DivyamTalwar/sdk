//! Incremental projection of exact-path volume query publications.

use crate::{
    Error, Result,
    context::PinnedContextPath,
    conversation::{ContentResidencyVerifier, VolumeRef, validate_content_path},
    resources::GenerationRef,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod compute;

/// Caller-owned bounds for query transfer and retained rendering data.
#[derive(Clone, Copy, Debug)]
pub struct DiffLimits {
    /// Total changed files retained for one volume.
    pub files: usize,
    /// Maximum exact-path changes in one publication.
    pub changes: usize,
    /// Maximum bytes read from either endpoint of one file.
    pub file_bytes: u64,
    /// Maximum retained unified patch bytes across the volume.
    pub render_bytes: usize,
    /// Input bytes and diff steps, including compared line bytes and traceback.
    pub work: usize,
}

/// One exact path in an authoritative volume query delta, relative to its volume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffPath {
    /// Canonical volume-relative path, including any declared subtree boundary.
    pub path: String,
    /// Pinned baseline content; absent for a path created since the fork base.
    pub base: Option<PinnedContextPath>,
    /// Pinned current content; absent for a removed path.
    pub current: Option<PinnedContextPath>,
}

/// A pushed volume query publication. It carries only paths affected by this
/// publication, while each path's baseline stays pinned to the fork base.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffPublication {
    /// Previous query generation; a mismatch requires snapshot recovery.
    pub previous: GenerationRef,
    /// Exact published generation, including for deletion-only publications.
    pub generation: GenerationRef,
    /// All affected paths, including both names of a rename and hard-link aliases.
    pub paths: Vec<DiffPath>,
}

/// Captured file status, independent of any terminal renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// Path did not exist at the fork base.
    Added,
    /// Path no longer exists.
    Removed,
    /// Both endpoints exist and differ.
    Modified,
}

/// Exact line ranges for one unified hunk. Lines remain in the unified patch,
/// once, rather than being duplicated into another attachment or text store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffHunk {
    /// One-based baseline start (zero for an insertion into an empty file).
    pub old_start: usize,
    /// Number of baseline lines covered.
    pub old_lines: usize,
    /// One-based resulting start.
    pub new_start: usize,
    /// Number of resulting lines covered.
    pub new_lines: usize,
}

/// Renderer-neutral file diff; binary changes are explicit and have no line counts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Added, removed or modified.
    pub kind: DiffKind,
    /// At least one side is non-UTF-8 or contains NUL.
    pub binary: bool,
    /// Inserted lines across all hunks, excluding context lines.
    pub added: u64,
    /// Removed lines across all hunks, excluding context lines.
    pub removed: u64,
    /// Exact hunk ranges.
    pub hunks: Vec<DiffHunk>,
    /// Standard unified patch, including no-newline markers.
    pub patch: String,
}

impl FileDiff {
    /// Computes a file against its immutable base. `None` means the path returned
    /// to its baseline; adding/removing an empty file remains a visible change.
    /// Formatting stops before the patch exceeds `maximum_render_bytes`.
    /// `maximum_work` bounds input scanning and computation, including traceback.
    pub fn between(
        base: Option<&[u8]>,
        current: Option<&[u8]>,
        maximum_render_bytes: usize,
        mut maximum_work: usize,
    ) -> Result<Option<Self>> {
        Self::between_work(base, current, maximum_render_bytes, &mut maximum_work)
    }

    fn between_work(
        base: Option<&[u8]>,
        current: Option<&[u8]>,
        maximum_render_bytes: usize,
        work: &mut usize,
    ) -> Result<Option<Self>> {
        let before = base.unwrap_or_default();
        let after = current.unwrap_or_default();
        compute::charge(
            work,
            before
                .len()
                .checked_add(after.len())
                .ok_or_else(compute::work_bound)?,
        )?;
        if base == current {
            return Ok(None);
        }
        let kind = match (base, current) {
            (None, _) => DiffKind::Added,
            (_, None) => DiffKind::Removed,
            _ => DiffKind::Modified,
        };
        let mut result = Self {
            kind,
            binary: true,
            added: 0,
            removed: 0,
            hunks: Vec::new(),
            patch: String::new(),
        };
        if before.contains(&0) || after.contains(&0) {
            return Ok(Some(result));
        }
        let (Ok(before), Ok(after)) = (std::str::from_utf8(before), std::str::from_utf8(after))
        else {
            return Ok(Some(result));
        };
        if before.len().abs_diff(after.len()) > maximum_render_bytes {
            return Err(render_bound());
        }
        result.binary = false;
        compute::text_diff(before, after, maximum_render_bytes, work, &mut result)?;
        Ok(Some(result))
    }
}

struct PatchText {
    text: String,
    maximum: usize,
}

impl std::fmt::Write for PatchText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if text.len() > self.maximum - self.text.len() {
            return Err(std::fmt::Error);
        }
        self.text.push_str(text);
        Ok(())
    }
}

fn render_bound() -> Error {
    Error::Invalid("diff view exceeds retained render bound".into())
}

/// Per-volume component of ClientViews. Query ownership and push subscriptions
/// stay with the Streams/filesystem client; this reducer opens no watcher or timer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiffView {
    volume: VolumeRef,
    base: GenerationRef,
    generation: GenerationRef,
    last_digest: Option<[u8; 32]>,
    files: BTreeMap<String, FileDiff>,
    added: u64,
    removed: u64,
    render_bytes: usize,
}

impl DiffView {
    /// Starts at the fork baseline. Reconnect reconstructs by applying a query
    /// snapshot as a publication from this base, then following its exact cursor.
    pub fn new(volume: VolumeRef, base: GenerationRef) -> Result<Self> {
        volume.validate(&mut None)?;
        base.validate(&mut None)?;
        if base.as_resource().provider() != volume.provider() {
            return Err(Error::Invalid(
                "diff base belongs to another provider".into(),
            ));
        }
        Ok(Self {
            volume,
            generation: base.clone(),
            base,
            last_digest: None,
            files: BTreeMap::new(),
            added: 0,
            removed: 0,
            render_bytes: 0,
        })
    }

    /// Borrows the current file projection without rebuilding it.
    pub fn files(&self) -> &BTreeMap<String, FileDiff> {
        &self.files
    }
    /// Volume whose fork changes this view renders.
    pub fn volume(&self) -> &VolumeRef {
        &self.volume
    }
    /// Immutable fork baseline used by every file endpoint.
    pub fn base(&self) -> &GenerationRef {
        &self.base
    }
    /// Total added and removed lines, maintained by delta.
    pub fn line_counts(&self) -> (u64, u64) {
        (self.added, self.removed)
    }
    /// Cursor generation to retain with the corresponding query follow.
    pub fn generation(&self) -> &GenerationRef {
        &self.generation
    }

    /// Consumes an owner-authorized publication query follow. Backpressure is the
    /// follow's demand; each publication is applied before asking for the
    /// next. A cursor gap or failed read terminates the follow without advancing
    /// this view, allowing the client to reopen its query at the retained cursor.
    pub fn follow<'a>(
        &'a mut self,
        publications: acyclic_stream::BoxProviderStream<'a, Result<DiffPublication>>,
        reader: &'a dyn ContentResidencyVerifier,
        limits: DiffLimits,
    ) -> DiffFollow<'a> {
        DiffFollow {
            view: self,
            publications,
            reader,
            limits,
            pending: None,
            ended: false,
        }
    }

    /// Applies one publication atomically. Only named files are read/recomputed;
    /// unchanged files are neither read nor traversed. Returns changed file names.
    pub async fn apply(
        &mut self,
        publication: &DiffPublication,
        reader: &dyn ContentResidencyVerifier,
        limits: DiffLimits,
    ) -> Result<Vec<String>> {
        if limits.files == 0
            || limits.changes == 0
            || limits.file_bytes == 0
            || limits.render_bytes == 0
            || limits.work == 0
            || publication.paths.len() > limits.changes
        {
            return Err(Error::Invalid("diff publication exceeds its bounds".into()));
        }
        publication.previous.validate(&mut None)?;
        publication.generation.validate(&mut None)?;
        if publication.generation.as_resource().provider() != self.volume.provider() {
            return Err(Error::Invalid(
                "diff publication belongs to another provider".into(),
            ));
        }
        let mut paths = BTreeSet::new();
        for change in &publication.paths {
            validate_content_path(&change.path, &mut None)?;
            if !paths.insert(&change.path) {
                return Err(Error::Conflict("duplicate diff path".into()));
            }
            for (source, generation) in [
                (&change.base, &self.base),
                (&change.current, &publication.generation),
            ] {
                if let Some(source) = source {
                    source.validate(&mut None)?;
                    if source.root.volume != self.volume
                        || source.path != change.path
                        || &source.generation != generation
                    {
                        return Err(Error::Invalid(
                            "diff file differs from its query endpoint".into(),
                        ));
                    }
                }
            }
        }
        let mut hasher = blake3::Hasher::new();
        serde_json::to_writer(&mut hasher, publication)
            .map_err(|error| Error::Invalid(error.to_string()))?;
        let digest = *hasher.finalize().as_bytes();
        if publication.generation == self.generation && self.last_digest == Some(digest) {
            return Ok(Vec::new());
        }
        if self.last_digest.is_none()
            && publication.previous == self.base
            && publication.generation == self.base
            && publication.paths.is_empty()
        {
            self.last_digest = Some(digest);
            return Ok(Vec::new());
        }
        if publication.previous != self.generation || publication.generation == self.generation {
            return Err(Error::Conflict(
                "diff query cursor mismatch; reopen snapshot".into(),
            ));
        }
        let mut updates = Vec::new();
        let mut work = limits.work;
        let (mut count, mut rendered, mut added, mut removed) = (
            self.files.len(),
            self.render_bytes,
            self.added,
            self.removed,
        );
        for change in &publication.paths {
            if let Some(old) = self.files.get(&change.path) {
                count -= 1;
                rendered -= old.patch.len();
                added -= old.added;
                removed -= old.removed;
            }
        }
        if count > limits.files || rendered > limits.render_bytes {
            return Err(Error::Invalid("diff view exceeds retained bounds".into()));
        }
        for change in &publication.paths {
            let before = read(&change.base, reader, limits.file_bytes).await?;
            let after = read(&change.current, reader, limits.file_bytes).await?;
            let diff = FileDiff::between_work(
                before.as_deref(),
                after.as_deref(),
                limits.render_bytes - rendered,
                &mut work,
            )?;
            if let Some(diff) = &diff {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| Error::Invalid("diff count overflow".into()))?;
                rendered = rendered
                    .checked_add(diff.patch.len())
                    .ok_or_else(|| Error::Invalid("diff size overflow".into()))?;
                added = added
                    .checked_add(diff.added)
                    .ok_or_else(|| Error::Invalid("diff lines overflow".into()))?;
                removed = removed
                    .checked_add(diff.removed)
                    .ok_or_else(|| Error::Invalid("diff lines overflow".into()))?;
                if count > limits.files {
                    return Err(Error::Invalid(
                        "diff view exceeds retained file bound".into(),
                    ));
                }
            }
            updates.push((change.path.clone(), diff));
        }
        let mut changed = Vec::new();
        for (path, diff) in updates {
            if self.files.get(&path) != diff.as_ref() {
                changed.push(path.clone());
                if let Some(diff) = diff {
                    self.files.insert(path, diff);
                } else {
                    self.files.remove(&path);
                }
            }
        }
        (self.added, self.removed, self.render_bytes) = (added, removed, rendered);
        self.generation = publication.generation.clone();
        self.last_digest = Some(digest);
        Ok(changed)
    }
}

/// A live diff subscription with an inspectable current view between reads.
pub struct DiffFollow<'a> {
    view: &'a mut DiffView,
    publications: acyclic_stream::BoxProviderStream<'a, Result<DiffPublication>>,
    reader: &'a dyn ContentResidencyVerifier,
    limits: DiffLimits,
    pending: Option<DiffPublication>,
    ended: bool,
}

impl DiffFollow<'_> {
    /// Borrows the current projection for a renderer without ending the follow.
    pub fn view(&self) -> &DiffView {
        self.view
    }

    /// Applies the next pushed publication. Cancelling this future retains any
    /// publication already received; an error ends the follow at the old cursor.
    pub async fn next(&mut self) -> Option<Result<Vec<String>>> {
        use futures::StreamExt as _;
        if self.ended {
            return None;
        }
        if self.pending.is_none() {
            match self.publications.next().await {
                Some(Ok(publication)) => self.pending = Some(publication),
                Some(Err(error)) => {
                    self.ended = true;
                    return Some(Err(error));
                }
                None => {
                    self.ended = true;
                    return None;
                }
            }
        }
        let result = self
            .view
            .apply(self.pending.as_ref()?, self.reader, self.limits)
            .await;
        self.pending = None;
        self.ended = result.is_err();
        Some(result)
    }
}

async fn read(
    source: &Option<PinnedContextPath>,
    reader: &dyn ContentResidencyVerifier,
    maximum_bytes: u64,
) -> Result<Option<acyclic_stream::OwnedBytes>> {
    match source {
        Some(source) => source
            .read(reader, maximum_bytes)
            .await
            .map(|(_, bytes)| Some(bytes)),
        None => Ok(None),
    }
}

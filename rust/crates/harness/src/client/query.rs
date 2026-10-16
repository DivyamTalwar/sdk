//! One disposable file-query fold with a retained, publish-before-cursor delivery.

use super::file_views::{FileViews, QueryViewLimits};
use crate::{
    Error, Result,
    task_volume::{Change, ChangeStream, Cursor, HeadFile, Query, TaskVolume},
};
use acyclic_stream::{StreamPath, StreamProvider};
use futures::StreamExt as _;

fn query_error(error: crate::task_volume::Error) -> Error {
    Error::Storage(error.to_string())
}

/// A single task-volume subscription shared by native and WASM renderers.
/// Source files stay authoritative; this cache is reconstructed at its cursor.
pub struct ClientQuery {
    target: StreamPath,
    query: Query,
    limits: QueryViewLimits,
    views: FileViews,
    changes: ChangeStream,
    pending: Option<Change>,
    reset: bool,
}

impl ClientQuery {
    /// Loads a snapshot at the retained cursor, or an explicit replacement cut
    /// when history cannot be resumed, then follows from that same cut.
    pub async fn open<P: StreamProvider + ?Sized>(
        volume: &TaskVolume<P>,
        query: Query,
        cursor: Option<Cursor>,
        limits: QueryViewLimits,
    ) -> Result<Self> {
        let observed = volume
            .observe(query.clone(), cursor.clone(), limits.segments)
            .await
            .map_err(query_error)?;
        let snapshot = match observed.snapshot {
            Some(snapshot) => snapshot,
            None => volume
                .snapshot_at(
                    &query,
                    cursor.ok_or_else(|| {
                        Error::Storage("query returned no snapshot or cursor".into())
                    })?,
                    limits.segments,
                )
                .await
                .map_err(query_error)?,
        };
        let views = FileViews::snapshot(snapshot.into_heads().map_err(query_error)?, limits)
            .map_err(query_error)?;
        Ok(Self {
            target: volume.path().clone(),
            query,
            limits,
            views,
            changes: observed.changes,
            pending: None,
            reset: observed.reset,
        })
    }

    /// Last completely published view cut. A failed publisher leaves it unchanged.
    pub fn cursor(&self) -> &Cursor {
        self.views.cursor()
    }

    /// Whether opening/resuming replaced an incompatible history rather than resuming it.
    pub fn reset(&self) -> bool {
        self.reset
    }

    /// Borrow one logical file, including ordered text/reasoning segments.
    pub fn file(&self, key: &crate::task_volume::FileKey) -> Option<&HeadFile> {
        self.views.file(key)
    }

    /// Visit retained files without copying their bytes or serializing the transcript.
    pub fn files(&self) -> impl Iterator<Item = &HeadFile> {
        self.views.files()
    }

    /// Publishes one exact query change before advancing the view/cursor. Validation
    /// happens first. Failed publication retains the same frame for retry, even
    /// when later records have already arrived at the provider.
    pub async fn publish_next<F>(&mut self, mut publish: F) -> Result<()>
    where
        F: for<'a> std::ops::AsyncFnMut(&'a Change) -> Result<()>,
    {
        if self.pending.is_none() {
            self.pending = Some(
                self.changes
                    .next()
                    .await
                    .ok_or_else(|| Error::Storage("query follow ended; reconnect required".into()))?
                    .map_err(query_error)?,
            );
        }
        let change = self
            .pending
            .as_ref()
            .ok_or_else(|| Error::Storage("query delivery missing".into()))?;
        let apply = self.views.prepare(change).map_err(query_error)?;
        publish(change).await?;
        apply();
        self.pending = None;
        Ok(())
    }

    /// Reconnects only after the pending frame has been published. Matching views
    /// resume directly; a reset atomically replaces them with a validated snapshot.
    pub async fn reconnect<P: StreamProvider + ?Sized>(
        &mut self,
        volume: &TaskVolume<P>,
    ) -> Result<bool> {
        if volume.path() != &self.target {
            return Err(Error::Invalid("query reconnect target differs".into()));
        }
        if self.pending.is_some() {
            return Err(Error::Conflict(
                "publish the buffered query delivery before reconnecting".into(),
            ));
        }
        let observed = volume
            .observe(
                self.query.clone(),
                Some(self.cursor().clone()),
                self.limits.segments,
            )
            .await
            .map_err(query_error)?;
        let replacement = observed
            .snapshot
            .map(|snapshot| FileViews::snapshot(snapshot.into_heads()?, self.limits))
            .transpose()
            .map_err(query_error)?;
        if let Some(views) = replacement {
            self.views = views;
        }
        self.changes = observed.changes;
        self.reset = observed.reset;
        Ok(observed.reset)
    }
}

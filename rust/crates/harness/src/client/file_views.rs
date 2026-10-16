//! Disposable file views at a query cursor. Bytes retain segment ownership.

use crate::task_volume::{Change, Cursor, Error, FileKey, HeadFile, HeadSnapshot, Result};
use std::collections::BTreeMap;

/// Bounds for the retained selected-agent query, including every segment.
#[derive(Clone, Copy)]
pub(super) struct FileViewLimits {
    pub files: usize,
    pub segments: usize,
    pub bytes: u64,
}

/// One agent's materialized query. Neither snapshots nor deltas concatenate text.
pub(super) struct FileViews {
    cursor: Cursor,
    files: BTreeMap<FileKey, HeadFile>,
    limits: FileViewLimits,
    segments: usize,
    bytes: u64,
}

impl FileViews {
    pub fn snapshot(snapshot: HeadSnapshot, limits: FileViewLimits) -> Result<Self> {
        if snapshot.cursor.schema != crate::task_volume::SCHEMA
            || snapshot.cursor.position == 0
            || snapshot.cursor.volume != snapshot.generation.volume
            || snapshot.cursor.epoch != snapshot.generation.epoch
            || snapshot.cursor.position != snapshot.generation.position
            || snapshot.cursor.commit != snapshot.generation.commit
            || snapshot.cursor.durable > snapshot.cursor.accepted
            || snapshot.cursor.position > snapshot.cursor.accepted
        {
            return Err(Error::ResetRequired);
        }
        let mut view = Self {
            cursor: snapshot.cursor,
            files: BTreeMap::new(),
            limits,
            segments: 0,
            bytes: 0,
        };
        for file in snapshot.files {
            let bytes = file.segments.iter().try_fold(0u64, |total, segment| {
                total.checked_add(segment.len() as u64).ok_or(Error::LimitExceeded)
            })?;
            if bytes != file.length || file.segments.is_empty() {
                return Err(Error::InvalidSegment);
            }
            let segments = view.segments.checked_add(file.segments.len()).ok_or(Error::LimitExceeded)?;
            let bytes = view.bytes.checked_add(bytes).ok_or(Error::LimitExceeded)?;
            view.admit(view.files.len().checked_add(1).ok_or(Error::LimitExceeded)?, segments, bytes)?;
            if view.files.contains_key(&file.key) {
                return Err(Error::ImmutableConflict(file.key));
            }
            view.files.insert(file.key.clone(), file);
            view.segments = segments;
            view.bytes = bytes;
        }
        Ok(view)
    }

    pub fn cursor(&self) -> &Cursor { &self.cursor }

    pub fn file(&self, key: &FileKey) -> Option<&HeadFile> { self.files.get(key) }

    pub fn files(&self) -> impl Iterator<Item = &HeadFile> { self.files.values() }

    fn admit(&self, files: usize, segments: usize, bytes: u64) -> Result<()> {
        if files > self.limits.files || segments > self.limits.segments || bytes > self.limits.bytes {
            return Err(Error::LimitExceeded);
        }
        Ok(())
    }

    /// Checks the entire change before changing either files or the applied cut.
    pub fn apply(&mut self, change: Change) -> Result<()> {
        let cursor = change.cursor;
        if cursor.volume != self.cursor.volume
            || cursor.epoch != self.cursor.epoch
            || cursor.schema != self.cursor.schema
            || cursor.query != self.cursor.query
            || self.cursor.position.checked_add(1) != Some(cursor.position)
            || cursor.accepted < self.cursor.accepted.max(cursor.position)
            || cursor.durable < self.cursor.durable
            || cursor.durable > cursor.accepted
        {
            return Err(Error::ResetRequired);
        }
        if let Some(file) = change.file {
            let (key, sequence) = file.head_key()?;
            let before = self.files.get(&key);
            if let Some(sequence) = sequence {
                if before.and_then(|head| head.through_sequence).is_some_and(|last| sequence <= last) {
                    return Err(Error::SequenceConflict(file.key));
                }
            } else if let Some(before) = before {
                if !file.key.mutable() && (before.segments.len() != 1 || before.segments[0] != file.bytes) {
                    return Err(Error::ImmutableConflict(file.key));
                }
            }
            let replaced_bytes = before.filter(|_| sequence.is_none()).map_or(0, |head| head.length);
            let replaced_segments = before.filter(|_| sequence.is_none()).map_or(0, |head| head.segments.len());
            let bytes = self.bytes.checked_sub(replaced_bytes)
                .and_then(|total| total.checked_add(file.bytes.len() as u64)).ok_or(Error::LimitExceeded)?;
            let segments = self.segments.checked_sub(replaced_segments)
                .and_then(|total| total.checked_add(1)).ok_or(Error::LimitExceeded)?;
            let count = self.files.len().checked_add(usize::from(before.is_none())).ok_or(Error::LimitExceeded)?;
            self.admit(count, segments, bytes)?;
            if let Some(sequence) = sequence {
                let head = self.files.entry(key.clone()).or_insert_with(|| HeadFile {
                    key, segments: Vec::new(), length: 0, through_sequence: None,
                });
                head.length = head.length.checked_add(file.bytes.len() as u64).ok_or(Error::LimitExceeded)?;
                head.segments.push(file.bytes);
                head.through_sequence = Some(sequence);
            } else {
                self.files.insert(key.clone(), HeadFile {
                    key, length: file.bytes.len() as u64, segments: vec![file.bytes], through_sequence: None,
                });
            }
            self.segments = segments;
            self.bytes = bytes;
        }
        self.cursor = cursor;
        Ok(())
    }
}

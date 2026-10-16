//! Disposable projections of GOAL.md and plans/ from volume snapshot/follow.

use crate::{
    Error, Result,
    task_volume::{Component, FileKey, FileValue, Query, Snapshot},
};
use serde::Serialize;
use std::collections::BTreeMap;

/// Document text is a view of ordinary private-volume files, never a second
/// goal/plan store. The volume query owns ordering, authorization and its cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DocumentViews {
    /// Latest GOAL.md, including its original Markdown/frontmatter.
    pub goal: Option<String>,
    /// Current plans, keyed by their canonical filename component.
    pub plans: BTreeMap<Component, String>,
}

impl DocumentViews {
    /// Replace only the selected files at an authenticated query's snapshot cut.
    /// Decode every selected document before changing the existing projection.
    pub fn replace(&mut self, query: &Query, snapshot: &Snapshot) -> Result<()> {
        let mut next = Self::default();
        for file in &snapshot.files {
            if !query.matches(&file.key) {
                return Err(Error::Conflict(
                    "document snapshot differs from its query".into(),
                ));
            }
            next.apply(file)?;
        }
        match query {
            Query::Root => *self = next,
            Query::Plans => self.plans = next.plans,
            Query::File(FileKey::Goal) => self.goal = next.goal,
            Query::File(FileKey::Plan(name)) => {
                if let Some(text) = next.plans.remove(name) {
                    self.plans.insert(name.clone(), text);
                } else {
                    self.plans.remove(name);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Fold one verified file from the same ordered follow used for the snapshot.
    /// Non-document files do not affect this projection. Failed decoding does
    /// not change the previous text; the caller must not advance its query cursor.
    pub fn apply(&mut self, file: &FileValue) -> Result<()> {
        if !matches!(file.key, FileKey::Goal | FileKey::Plan(_)) {
            return Ok(());
        }
        let text = std::str::from_utf8(&file.bytes)
            .map_err(|error| Error::Invalid(error.to_string()))?
            .to_owned();
        match &file.key {
            FileKey::Goal => self.goal = Some(text),
            FileKey::Plan(name) => {
                self.plans.insert(name.clone(), text);
            }
            _ => {}
        }
        Ok(())
    }
}

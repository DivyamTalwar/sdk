//! Disposable client views over the authorized task-volume snapshot and follow.
use super::{ClientSession, ClientViews, MemoryClientState};
use crate::{
    Error, Result,
    task_volume::{ChangeStream, FileQuerySource, Query},
    wire,
};
use futures::StreamExt as _;

/// File-backed client connection; cursors and payloads come from the volume query.
pub struct TaskFileClient {
    session: ClientSession<MemoryClientState>,
    changes: ChangeStream,
    command_cut: Option<(u64, Option<crate::task_volume::FileValue>)>,
}
impl TaskFileClient {
    /// Observe an authorized task selection at one atomic cut.
    pub async fn observe(
        files: &impl FileQuerySource,
        owner: &wire::Authority,
        maximum_files: usize,
        maximum_bytes: usize,
    ) -> Result<Self> {
        let mut observation = files
            .observe_files(Query::Tasks, None, maximum_files, maximum_bytes)
            .await?;
        let snapshot = observation
            .snapshot
            .take()
            .ok_or_else(|| Error::Conflict("task snapshot missing".into()))?;
        let mut session = ClientSession::open(MemoryClientState::default())?;
        session.track(wire::ReplayCursor {
            authority: Some(owner.clone()),
            generation: "task-files".into(),
            revision: 0,
        })?;
        session.query_snapshot(Query::Tasks, snapshot, maximum_files, maximum_bytes)?;
        Ok(Self {
            session,
            changes: observation.changes,
            command_cut: None,
        })
    }
    /// Presentation state reconstructed from the followed files.
    pub fn views(&self) -> &ClientViews {
        self.session.views()
    }
    /// Project this UI session's completed commands into its shared transcript.
    pub fn project_commands<P: acyclic_stream::StreamProvider + ?Sized>(
        &mut self,
        files: &super::CommandSession<P>,
        owner: &wire::Authority,
    ) -> Result<()> {
        let cut = (
            files.output_sequence(),
            files
                .state_file(super::CommandFileRoot::Session, "layout.json")?
                .cloned(),
        );
        if self.command_cut.as_ref() != Some(&cut) {
            if cut.0 > 0 {
                self.session.project_commands(files, owner)?;
            }
            self.command_cut = Some(cut);
        }
        Ok(())
    }

    /// Apply a separate authorized document query.
    pub fn apply_update(&mut self, update: super::ViewUpdate) -> Result<()> {
        self.session.apply_update(update)
    }
    /// Read the next push publication; no local polling or execution host.
    pub async fn next_change(&mut self) -> Result<()> {
        let change = self
            .changes
            .next()
            .await
            .ok_or_else(|| Error::Conflict("task follow ended; reopen snapshot".into()))??;
        self.session.query_change(change)?;
        self.command_cut = None;
        Ok(())
    }
}

#[cfg(all(test, not(acyclic_harness_loom)))]
mod tests;

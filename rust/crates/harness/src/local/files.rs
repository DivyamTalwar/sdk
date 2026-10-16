use super::*;
use crate::filesystem::{FilesystemContentVerifier, FilesystemFileQuery};

impl LocalHarness {
    /// Read-only live queries over the agent's actual private SDK files.
    /// The existing verifier retains owner authority; execution-log files and
    /// the host journal remain distinct volumes.
    pub fn scratch_files(
        &self,
        budget: acyclic_fs::WorkBudget,
    ) -> Result<FilesystemFileQuery<LocalStream, LocalObjectBackend>> {
        FilesystemContentVerifier::new(
            Arc::clone(&self.files),
            &self.verifier,
            &self.signed,
            self.limits.file_bytes,
            None,
        )?
        .file_query(&self.scratch, budget)
    }
}

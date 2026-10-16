use super::*;

impl LocalHarness {
    pub(super) fn goal_task_path(&self) -> Option<&'static str> {
        ["GOAL.md", ".volumes/scratch/GOAL.md"]
            .into_iter()
            .find(|path| {
                self.runtime
                    .harness()
                    .output_destination(path)
                    .is_some_and(|(volume, relative)| {
                        volume == &self.scratch && relative == "GOAL.md"
                    })
            })
    }

    pub(super) async fn block_goal(
        &self,
        goal: &GoalView,
        settled: OperationId,
        contents: &str,
    ) -> Result<crate::conversation::FileRef> {
        let grant = ContentGrant::verify(
            &self.verifier,
            &self.signed,
            &self.scratch,
            VolumeOperation::Write,
            u64::MAX,
            &mut None,
        )?;
        self.files
            .put_content(
                &self.scratch,
                &grant,
                "GOAL.md",
                contents.as_bytes(),
                "text/markdown",
                "GOAL.md",
                self.limits.file_bytes,
                &crate::IdempotencyKey::new(format!(
                    "goal-block:{settled}:{}",
                    goal.source.version()
                ))?,
                Some(&self.files.file_generation(&goal.source, None)?),
                None,
            )
            .await
    }

    pub(super) async fn record_goal_revision(
        &self,
        current: &Turn,
        generation: &GenerationRef,
        goal: &crate::conversation::FileRef,
    ) -> Result<()> {
        let revision = *goal.descriptor().sha256();
        if current.goal_revision == Some(revision) {
            return Ok(());
        }
        let mut next = current.clone();
        next.goal_revision = Some(revision);
        self.write_turn(
            &next,
            generation,
            &crate::IdempotencyKey::new(format!(
                "goal-observed:{}:{}",
                current.operation,
                goal.version()
            ))?,
        )
        .await
    }
}

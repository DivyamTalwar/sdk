use super::super::snapshot::Snapshot;
use super::*;

pub(super) struct Bootstrap {
    source: Root,
    provider: Snapshot,
    state: Snapshot,
    volume: VolumeRef,
    pub agent: Option<AgentId>,
}

pub(super) struct Root {
    pub directory: tempfile::TempDir,
    state: PathBuf,
}

impl Drop for Root {
    fn drop(&mut self) {
        // state is the exact per-user hash for this uniquely created test root.
        if self.state.exists() {
            std::fs::remove_dir_all(&self.state).expect("remove test state");
        }
    }
}

impl Bootstrap {
    pub async fn new() -> Result<Self> {
        let directory = tempfile::tempdir().map_err(storage)?;
        let volume = super::super::super::tests::volume(directory.path()).await?;
        let provider = Arc::new(ScriptedModelProvider::new([]));
        let config =
            super::super::super::tests::volume_config(directory.path(), volume.clone(), provider)?
                .compaction(CompactionPolicy::Disabled);
        let state = super::super::super::owner::state_directory(&config.project)?;
        let source = Root { directory, state };
        let harness = LocalHarness::open(config).await?;
        let agent = harness.signed.agent();
        assert!(harness.current_turn().await?.0.is_none());
        drop(harness);
        Ok(Self {
            provider: Snapshot::capture(source.directory.path())?,
            state: Snapshot::capture(&source.state)?,
            source,
            volume,
            agent,
        })
    }

    pub fn restore(&self) -> Result<(Root, Arc<ScriptedModelProvider>)> {
        let parent = tempfile::tempdir().map_err(storage)?;
        let directory = parent.path().join("root");
        self.provider.restore(&directory)?;
        let model = Arc::new(ScriptedModelProvider::new([ScriptedTurn::Text(
            "done".into(),
        )]));
        let config = self.config(&directory, model.clone())?;
        let state = super::super::super::owner::state_directory(&config.project)?;
        assert_ne!(state, self.source.state);
        self.state.restore(&state)?;
        Ok((
            Root {
                directory: parent,
                state,
            },
            model,
        ))
    }

    pub fn config(
        &self,
        root: &Path,
        model: Arc<ScriptedModelProvider>,
    ) -> Result<LocalHarnessConfig> {
        let mut config =
            super::super::super::tests::volume_config(root, self.volume.clone(), model)?
                .compaction(CompactionPolicy::Disabled);
        config.goal_continuations = None;
        Ok(config)
    }
}

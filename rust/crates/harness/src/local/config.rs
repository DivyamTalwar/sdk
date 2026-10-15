use super::*;

/// Storage selected for the agent's project view.
pub enum LocalProject {
    /// A normal checkout, opened lazily without importing its files.
    Workspace(PathBuf),
    /// An existing project volume in the configured local filesystem.
    Volume(VolumeRef),
}

/// Durable local composition. Custom tools are additive to the default hook.
pub struct LocalHarnessConfig {
    pub(super) directory: PathBuf,
    pub(super) project: LocalProject,
    pub(super) model: Option<(Model, Arc<dyn ModelProvider>)>,
    pub(super) context: ContextPipeline,
    pub(super) defaults: Vec<Tool>,
    pub(super) tools: Vec<Tool>,
    pub(super) default_tools: bool,
    /// Content, projection and model-loop bounds.
    pub limits: Limits,
    /// Durable task execution bounds.
    pub run_limits: TaskRunLimits,
    /// Durable session admission bounds.
    pub session_limits: SessionLimits,
    /// Maximum number of concurrently running local tasks.
    pub concurrency: usize,
    /// Stock context compaction policy.
    pub compaction: crate::context::CompactionPolicy,
}

impl LocalHarnessConfig {
    /// Opens a checkout and stores durable state under `.acyclic/harness`.
    #[must_use]
    pub fn workspace(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self::new(path.join(".acyclic/harness"), LocalProject::Workspace(path))
    }

    /// Opens an existing project volume in `directory/filesystem`.
    #[must_use]
    pub fn volume(directory: impl Into<PathBuf>, volume: VolumeRef) -> Self {
        Self::new(directory.into(), LocalProject::Volume(volume))
    }

    fn new(directory: PathBuf, project: LocalProject) -> Self {
        Self {
            directory,
            project,
            model: None,
            context: ContextPipeline::default(),
            defaults: Vec::new(),
            tools: Vec::new(),
            default_tools: true,
            limits: Limits::default(),
            run_limits: TaskRunLimits::default(),
            session_limits: SessionLimits {
                active_tasks: 16,
                total_tasks: 4096,
                depth: 16,
                model_steps: 65_536,
            },
            concurrency: 1,
            compaction: crate::context::CompactionPolicy::default(),
        }
    }

    /// Selects a durable state directory independently of the checkout.
    #[must_use]
    pub fn state_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.directory = directory.into();
        self
    }

    /// Binds the stock executor's model and streaming provider.
    #[must_use]
    pub fn model(mut self, model: Model, provider: Arc<dyn ModelProvider>) -> Self {
        self.model = Some((model, provider));
        self
    }

    /// Selects the stock model's context pipeline.
    #[must_use]
    pub fn context(mut self, context: ContextPipeline) -> Self {
        self.context = context;
        self
    }

    /// Supplies the exported coding toolset hook, filled by the tools composition.
    #[must_use]
    pub fn default_tools(mut self, tools: Vec<Tool>) -> Self {
        self.defaults = tools;
        self
    }

    /// Disables the default set, retaining every custom tool.
    #[must_use]
    pub fn without_default_tools(mut self) -> Self {
        self.default_tools = false;
        self
    }

    /// Adds custom tools to the enabled default set.
    #[must_use]
    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools.extend(tools);
        self
    }
}

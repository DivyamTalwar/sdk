use super::*;
use crate::{
    conversation::ContentPublisher,
    tool::files::{FileResult, WriteFileInput},
};
use std::sync::OnceLock;

// A caller-selected ordinary filesystem writer. The local default `write`
// targets the project; these scenarios explicitly select the private volume.
#[derive(Clone, Default)]
pub(super) struct Files {
    writer: Arc<OnceLock<Arc<dyn ContentPublisher>>>,
}

impl Files {
    pub(super) fn configure(&self, config: LocalHarnessConfig) -> Result<LocalHarnessConfig> {
        let defaults = crate::tools::defaults()?;
        let mut tools = Vec::new();
        for definition in defaults.definitions(None)? {
            let mut tool = defaults
                .get(&definition.name, Some(&definition.revision), None)?
                .expect("default definition has an executor")
                .clone();
            if definition.name == "write" {
                tool.executor = Arc::new(self.clone());
            }
            tools.push(tool);
        }
        // Keep the exact stock catalog and accounting; replace only its writer's
        // destination rather than adding a second copy of the write schema.
        Ok(config.without_default_tools().tools(tools))
    }

    pub(super) fn bind(&self, harness: &LocalHarness) -> Result<()> {
        let writer = crate::filesystem::FilesystemContentPublisher::new(
            harness.files.clone(),
            harness.scratch(),
            &harness.verifier,
            &harness.signed,
            harness.limits.file_bytes,
            None,
        )?;
        self.writer
            .set(writer)
            .map_err(|_| Error::Conflict("filesystem writer already bound".into()))
    }

    fn publisher(&self) -> Result<&Arc<dyn ContentPublisher>> {
        self.writer
            .get()
            .ok_or_else(|| Error::Unsupported("filesystem writer is not bound".into()))
    }
}

impl ToolExecutor for Files {
    fn authorize(
        &self,
        _: Option<&crate::runtime::RuntimeScope>,
        context: Option<&ToolContext>,
        invocation: &ToolInvocation,
        work: crate::NativeWork<'_>,
    ) -> Result<()> {
        if work.is_some() {
            return Err(Error::Unsupported(
                "fixture requires ordinary execution".into(),
            ));
        }
        let context =
            context.ok_or_else(|| Error::Unauthorized("file write requires a task".into()))?;
        let grant = self
            .publisher()?
            .volume()
            .capability(VolumeOperation::Write, &mut None)?;
        if !context.effective_scope().grants().contains(&grant)
            || context.operation_id() != invocation.operation_id
            || context.call_id() != invocation.call_id
        {
            return Err(Error::Unauthorized(
                "private write differs from its task authority".into(),
            ));
        }
        Ok(())
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            self.authorize(None, context.as_ref(), &invocation, None)?;
            let input: WriteFileInput = serde_json::from_value(invocation.arguments)
                .map_err(super::super::super::storage)?;
            let limits = context.as_ref().unwrap().effective_scope().limits();
            if input.text.len() as u64 > limits.file_bytes || input.path.len() > limits.path_bytes {
                return Err(Error::Invalid("file exceeds task content limits".into()));
            }
            let file = self
                .publisher()?
                .stage_once(
                    invocation.operation_id,
                    &input.path,
                    input.text.as_bytes(),
                    &input.media_type,
                    &input.display_name,
                    None,
                )?
                .await?
                .0;
            Ok(ToolResult {
                value: serde_json::to_value(FileResult { file })
                    .map_err(super::super::super::storage)?,
            })
        })
    }

    fn reconcile<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Option<ToolResult>>> {
        Box::pin(async move {
            self.authorize(None, context.as_ref(), &invocation, None)?;
            let (file, _) = self
                .publisher()?
                .observe_staged_once(
                    invocation.operation_id,
                    context
                        .as_ref()
                        .unwrap()
                        .effective_scope()
                        .limits()
                        .file_bytes,
                    None,
                )?
                .await?;
            file.map(|file| {
                serde_json::to_value(FileResult { file })
                    .map(|value| ToolResult { value })
                    .map_err(super::super::super::storage)
            })
            .transpose()
        })
    }
}

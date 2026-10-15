//! Keep the original command diagnostic before stock reconciliation classifies it.
use super::*;

struct Checked(Arc<dyn ToolExecutor>);

impl ToolExecutor for Checked {
    fn authorize(
        &self,
        scope: Option<&crate::runtime::RuntimeScope>,
        context: Option<&ToolContext>,
        invocation: &ToolInvocation,
        work: crate::NativeWork<'_>,
    ) -> Result<()> {
        self.0.authorize(scope, context, invocation, work)
    }
    fn execute<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            let arguments = invocation.arguments.clone();
            let result = self.0.execute(invocation, context).await;
            assert!(result.is_ok(), "checkout {arguments}: {result:?}");
            result
        })
    }
    fn reconcile<'a>(
        &'a self,
        invocation: ToolInvocation,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Option<ToolResult>>> {
        self.0.reconcile(invocation, context)
    }
}

pub(super) fn defaults() -> Result<Vec<Tool>> {
    let registry = crate::tools::defaults()?;
    registry
        .definitions(None)?
        .into_iter()
        .map(|definition| {
            let mut tool = registry
                .get(&definition.name, None, None)?
                .ok_or_else(|| Error::NotFound(definition.name.clone()))?
                .clone();
            if matches!(definition.name.as_str(), "bash" | "fs") {
                tool.executor = Arc::new(Checked(tool.executor));
            }
            Ok(tool)
        })
        .collect()
}

use super::*;

#[test]
fn defaults_are_exported_additive_and_explicitly_disabled() -> Result<()> {
    let defaults = crate::tools::defaults()?;
    let names = ["bash", "fs", "read", "write", "edit"];
    for name in names {
        assert_eq!(
            defaults
                .get(name, None, None)?
                .map(|tool| tool.definition.name.as_str()),
            Some(name)
        );
    }
    let mut custom = ToolRegistry::default();
    custom.register((example_tool(), None), &mut None, None)?;
    let mut second = example_tool();
    second.definition.name = "example.second".into();
    let enabled = HarnessBuilder::new()
        .agent_loop(Arc::new(DelegatingLoop))
        .tool(second)?
        .tools(custom)?
        .build()?;
    for name in names {
        assert_eq!(enabled.runtime().tool(name)?.name, name);
        assert!(
            enabled
                .capabilities()
                .contains(&format!("tool:call:{name}"))
        );
    }
    assert_eq!(enabled.runtime().tool("example.echo")?.name, "example.echo");
    assert_eq!(
        enabled.runtime().tool("example.second")?.name,
        "example.second"
    );
    let disabled = HarnessBuilder::new()
        .agent_loop(Arc::new(DelegatingLoop))
        .default_tools(false)
        .tool(example_tool())?
        .build()?;
    for name in names {
        assert!(matches!(
            disabled.runtime().tool(name),
            Err(Error::NotFound(_))
        ));
        assert!(
            !disabled
                .capabilities()
                .contains(&format!("tool:call:{name}"))
        );
    }
    assert_eq!(
        disabled.runtime().tool("example.echo")?.name,
        "example.echo"
    );
    Ok(())
}

#[test]
fn defaults_cannot_replace_custom_tools_or_broaden_bound_authority() -> Result<()> {
    assert!(matches!(
        HarnessBuilder::new()
            .agent_loop(Arc::new(DelegatingLoop))
            .tool(crate::tool::files::read()?)?
            .build(),
        Err(Error::Conflict(_))
    ));
    let bound = HarnessBuilder::new()
        .bindings(Bindings::local())
        .agent_loop(Arc::new(DelegatingLoop))
        .build()?;
    for name in ["bash", "fs", "read", "write", "edit"] {
        assert_eq!(bound.runtime().tool(name)?.name, name);
        assert!(!bound.capabilities().contains(&format!("tool:call:{name}")));
    }
    Ok(())
}

#[test]
fn registry_merge_rejects_duplicates_before_mutating_destination() -> Result<()> {
    let mut destination = crate::tools::defaults()?;
    let mut incoming = ToolRegistry::default();
    incoming.register((example_tool(), None), &mut None, None)?;
    incoming.register((crate::tool::files::read()?, None), &mut None, None)?;
    assert!(matches!(
        destination.extend(incoming),
        Err(Error::Conflict(_))
    ));
    assert!(destination.get("example.echo", None, None)?.is_none());
    assert!(destination.get("read", None, None)?.is_some());
    Ok(())
}

#[tokio::test]
async fn workspace_commands_use_the_calling_runtime_without_approval() -> Result<()> {
    struct Workspace(&'static str);
    impl ToolExecutor for Workspace {
        fn execute<'a>(
            &'a self,
            invocation: ToolInvocation,
            context: Option<crate::runtime::ToolContext>,
        ) -> BoxFuture<'a, Result<ToolResult>> {
            Box::pin(async move {
                let context = context.ok_or_else(|| Error::Unauthorized("missing task".into()))?;
                assert_eq!(context.operation_id(), invocation.operation_id);
                assert_eq!(context.call_id(), invocation.call_id);
                Ok(ToolResult {
                    value: json!({"workspace":self.0,"command":invocation.arguments["command"],"tool":invocation.name}),
                })
            })
        }
        fn reconcile<'a>(
            &'a self,
            _: ToolInvocation,
            _: Option<crate::runtime::ToolContext>,
        ) -> BoxFuture<'a, Result<Option<ToolResult>>> {
            Box::pin(async { Ok(None) })
        }
    }
    for workspace in ["checkout", "child-view"] {
        let bundle = HarnessBuilder::new()
            .agent_loop(Arc::new(DelegatingLoop))
            .workspace_tools(Arc::new(Workspace(workspace)))
            .grant("task:spawn:commands@1")
            .task(TaskDefinition::live(
                "commands",
                "1",
                move |context, (): ()| async move {
                    for (name, command) in
                        [("bash", "printf captured"), ("fs", "diff agents/child")]
                    {
                        let result: Value = context
                            .call(&context.tool(name)?, json!({"command":command}))
                            .await?;
                        assert_eq!(result["tool"], name);
                        assert_eq!(result["command"], command);
                        assert_eq!(result["workspace"], workspace);
                    }
                    Ok(())
                },
            )?)?
            .build()?;
        let task = bundle.runtime().task::<(), ()>("commands")?;
        assert_eq!(
            bundle.runtime().spawn(&task, ()).await?.result().await?,
            crate::Outcome::Succeeded(())
        );
    }
    Ok(())
}

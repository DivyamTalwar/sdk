//! Local defaults, durable stock turns and checkout selection through the public API.
#![cfg(feature = "filesystem-local")]

use acyclic_harness::{
    Admission, Error, LocalHarness, LocalHarnessConfig, OperationId, Result,
    context::{ModelContextCapacity, ModelTokenCount},
    conversation::{VolumeClass, VolumeOperation, VolumeOwner, VolumeRef},
    filesystem::FilesystemHost,
    model::{Model, ModelDispatch, ModelEvent, ModelProvider, PreparedModelRequest},
    resources::ProviderRef,
    runtime::{TaskStateProvider, ToolContext},
    tool::{Tool, ToolDefinition, ToolExecutor, ToolInvocation, ToolProjection, ToolResult},
};
use acyclic_stream::{BoxProviderFuture, BoxProviderStream};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Scripted {
    script: Mutex<Vec<Vec<ModelEvent>>>,
    requests: AtomicUsize,
}
impl Scripted {
    fn text() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(vec![vec![
                ModelEvent::Content {
                    delta: "done".into(),
                },
                ModelEvent::Completed {
                    metadata: json!({}),
                },
            ]]),
            requests: AtomicUsize::new(0),
        })
    }
}
impl ModelProvider for Scripted {
    fn context_capacity(&self, _: &Model) -> Result<ModelContextCapacity> {
        Ok(ModelContextCapacity {
            context_tokens: 32_768,
            output_tokens: 4096,
        })
    }
    fn count_tokens(&self, request: &PreparedModelRequest) -> Result<ModelTokenCount> {
        Ok(ModelTokenCount {
            request_digest: request.request_digest(),
            fixed_tokens: 8,
            message_tokens: vec![8; request.request().messages.len()],
        })
    }
    fn generate<'a>(
        &'a self,
        _: PreparedModelRequest,
        _: ModelDispatch,
    ) -> BoxProviderStream<'a, Result<ModelEvent>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let events = match self.script.lock() {
            Ok(mut script) if !script.is_empty() => script.remove(0).into_iter().map(Ok).collect(),
            _ => vec![Err(Error::Conflict("script exhausted".into()))],
        };
        Box::pin(futures::stream::iter(events))
    }
}

fn model() -> Result<Model> {
    Model::new("scripted".into(), "test".into(), "1".into(), Value::Null)
}

struct Echo;
impl ToolExecutor for Echo {
    fn execute<'a>(
        &'a self,
        _: ToolInvocation,
        _: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<ToolResult>> {
        Box::pin(async {
            Ok(ToolResult {
                value: json!({"text":"echo"}),
            })
        })
    }
    fn reconcile<'a>(
        &'a self,
        _: ToolInvocation,
        _: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Option<ToolResult>>> {
        Box::pin(async { Ok(None) })
    }
}
impl ToolProjection for Echo {
    fn project(
        &self,
        _: &ToolInvocation,
        result: &ToolResult,
        _: acyclic_harness::NativeWork<'_>,
    ) -> Result<(Value, acyclic_harness::NativeCustody)> {
        Ok((result.value.clone(), None))
    }
}
fn tool(name: &str) -> Tool {
    Tool {
        definition: ToolDefinition {
            name: name.into(),
            revision: "1".into(),
            description: "echo".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            projection_schema: acyclic_harness::tool::json_projection_schema(
                json!({"type":"object"}),
            ),
        },
        executor: Arc::new(Echo),
        projection: Arc::new(Echo),
    }
}

async fn volume(directory: &Path) -> Result<VolumeRef> {
    acyclic_native_runtime::initialize_native_io_services()
        .map_err(|error| Error::Storage(error.to_string()))?;
    let provider = ProviderRef::new(
        "local-test".into(),
        "filesystem".into(),
        "2".into(),
        &mut None,
    )?;
    let volume = VolumeRef::new(
        provider.clone(),
        "project".into(),
        VolumeClass::Project,
        VolumeOwner::Project("test".into()),
        &mut None,
    )?;
    let fs = acyclic_fs::Fs::local(acyclic_fs::LocalOptions::new(directory.join("filesystem")))
        .await
        .map_err(|error| Error::Storage(error.to_string()))?;
    FilesystemHost::new(fs, &provider, None)?
        .create_volume(&volume)
        .await?;
    Ok(volume)
}

#[tokio::test]
async fn defaults_are_additive_and_opt_out_retains_custom_grants() -> Result<()> {
    for defaults in [true, false] {
        let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
        let project = volume(directory.path()).await?;
        let mut config = LocalHarnessConfig::volume(directory.path(), project)
            .model(model()?, Scripted::text())
            .default_tools(vec![tool("test.default")])
            .tools(vec![tool("test.custom")]);
        if !defaults {
            config = config.without_default_tools();
        }
        let harness = LocalHarness::open(config).await?;
        assert_eq!(
            harness.tools().get("test.default", None, None)?.is_some(),
            defaults
        );
        assert!(harness.tools().get("test.custom", None, None)?.is_some());
        let Admission::Accepted(acyclic_harness::runtime::RuntimeTask::Durable { task_id, .. }) =
            harness.submit(OperationId::new(), "inspect grants").await?
        else {
            return Err(Error::Conflict("stock admission rejected".into()));
        };
        let admission = harness
            .runtime()
            .task_host()
            .observe_admission(task_id)
            .await?;
        assert_eq!(
            admission.grants.contains("tool:call:test.default"),
            defaults
        );
        assert!(admission.grants.contains("tool:call:test.custom"));
        assert!(
            admission.grants.contains(
                &harness
                    .journal_volume()
                    .directory_read_capability("inputs", &mut None)?
            )
        );
        assert!(
            !admission.grants.contains(
                &harness
                    .journal_volume()
                    .directory_read_capability("", &mut None)?
            )
        );
        assert!(
            !admission.grants.contains(
                &harness
                    .journal_volume()
                    .capability(VolumeOperation::Read, &mut None)?
            )
        );
        assert!(
            harness.owner_scope().capabilities().contains(
                &harness
                    .journal_volume()
                    .capability(VolumeOperation::Write, &mut None)?
            )
        );
    }
    Ok(())
}

#[tokio::test]
async fn reopen_retains_owner_and_resumes_admitted_stock_turn_once() -> Result<()> {
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let project = volume(directory.path()).await?;
    let provider = Scripted::text();
    let config = || {
        Ok::<_, Error>(
            LocalHarnessConfig::volume(directory.path(), project.clone())
                .model(model()?, provider.clone()),
        )
    };
    let harness = LocalHarness::open(config()?).await?;
    let operation = OperationId::new();
    let Admission::Accepted(acyclic_harness::runtime::RuntimeTask::Durable {
        task_id: task, ..
    }) = harness.submit(operation, "finish this task").await?
    else {
        return Err(Error::Conflict("stock admission rejected".into()));
    };
    let scope = harness.owner_scope().clone();
    drop(harness);
    let reopened = LocalHarness::open(config()?).await?;
    assert_eq!(reopened.owner_scope(), &scope);
    assert_eq!(reopened.resume(task).await?.text, "done");
    assert_eq!(reopened.resume(task).await?.text, "done");
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn duplicate_default_and_custom_contracts_are_rejected() -> Result<()> {
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let project = volume(directory.path()).await?;
    let config = LocalHarnessConfig::volume(directory.path(), project)
        .model(model()?, Scripted::text())
        .default_tools(vec![tool("test.echo")])
        .tools(vec![tool("test.echo")]);
    assert!(LocalHarness::open(config).await.is_err());
    Ok(())
}

#[tokio::test]
async fn stock_executor_calls_default_and_custom_tools() -> Result<()> {
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let project = volume(directory.path()).await?;
    let provider = Arc::new(Scripted {
        script: Mutex::new(vec![
            vec![
                ModelEvent::ToolCall {
                    call_id: "default".into(),
                    name: "test.default".into(),
                    arguments: json!({}),
                },
                ModelEvent::ToolCall {
                    call_id: "custom".into(),
                    name: "test.custom".into(),
                    arguments: json!({}),
                },
                ModelEvent::Completed {
                    metadata: json!({}),
                },
            ],
            vec![
                ModelEvent::Content {
                    delta: "both tools ran".into(),
                },
                ModelEvent::Completed {
                    metadata: json!({}),
                },
            ],
        ]),
        requests: AtomicUsize::new(0),
    });
    let harness = LocalHarness::open(
        LocalHarnessConfig::volume(directory.path(), project)
            .model(model()?, provider.clone())
            .default_tools(vec![tool("test.default")])
            .tools(vec![tool("test.custom")]),
    )
    .await?;
    assert_eq!(harness.run("call both tools").await?.text, "both tools ran");
    assert_eq!(provider.requests.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn corrupt_owner_is_rejected_without_reissuing_credentials() -> Result<()> {
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let project = volume(directory.path()).await?;
    let config = || {
        Ok::<_, Error>(
            LocalHarnessConfig::volume(directory.path(), project.clone())
                .model(model()?, Scripted::text()),
        )
    };
    drop(LocalHarness::open(config()?).await?);
    let path = directory.path().join("owner.json");
    std::fs::write(&path, b"invalid owner").map_err(|error| Error::Storage(error.to_string()))?;
    assert!(matches!(
        LocalHarness::open(config()?).await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        std::fs::read(&path).map_err(|error| Error::Storage(error.to_string()))?,
        b"invalid owner"
    );
    Ok(())
}

#[cfg(feature = "native-execution")]
#[tokio::test]
async fn checkout_is_lazy_and_state_cannot_be_repointed() -> Result<()> {
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let checkout = directory.path().join("checkout");
    let state = directory.path().join("state");
    std::fs::create_dir(&checkout).map_err(|error| Error::Storage(error.to_string()))?;
    std::fs::write(checkout.join("hello.txt"), "original checkout")
        .map_err(|error| Error::Storage(error.to_string()))?;
    let config = || {
        Ok::<_, Error>(
            LocalHarnessConfig::workspace(&checkout)
                .state_directory(&state)
                .model(model()?, Scripted::text()),
        )
    };
    let harness = LocalHarness::open(config()?).await?;
    let workspace = harness
        .workspace()
        .ok_or_else(|| Error::NotFound("checkout view".into()))?;
    let cancellation = acyclic_fs::CancellationToken::new();
    let work = acyclic_fs::NativeMaterializationWork::new(
        acyclic_fs::WorkBudget::UNBOUNDED,
        &cancellation,
    )
    .map_err(|(cause, work)| Error::NativeConstruction { cause, work })?;
    let mut accounting = acyclic_fs::kernel::KernelAccounting::NativeOriginal {
        work: &work,
        cancellation: &cancellation,
    };
    let bytes = workspace
        .view()
        .read("/hello.txt", 1024, &mut accounting)
        .await
        .map_err(|error| Error::Storage(error.to_string()))?;
    assert_eq!(bytes.bytes.as_ref(), b"original checkout");
    drop(bytes);
    let project = harness.project().clone();
    drop(harness);
    let reopened = LocalHarness::open(config()?).await?;
    assert_eq!(reopened.project(), &project);
    let other = LocalHarnessConfig::workspace(directory.path())
        .state_directory(&state)
        .model(model()?, Scripted::text());
    assert!(matches!(
        LocalHarness::open(other).await,
        Err(Error::Unauthorized(_))
    ));
    Ok(())
}

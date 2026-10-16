use super::*;
use acyclic_harness::{
    Admission, AgentId, Capabilities, OperationId,
    context::{CompactionPolicy, ContextPipeline},
    conversation::{VolumeClass, VolumeOperation, VolumeOwner, VolumeRef},
    core::{AggregateKind, Authority, AuthorityIssuer},
    distributed::{SchedulerPayloadStore, WorkPull, Worker},
    durable_tool::ResumableToolRegistry,
    filesystem::{
        FilesystemContentPublisher, FilesystemHost, FilesystemTaskRuntime, MODEL_TASK_COMMAND_KIND,
        ModelTaskCommand, PreparationIo, TaskWorkerAttempt, TaskWorkerOutcome,
    },
    resources::ProviderRef,
    runtime::{RuntimeScope, TaskDefinition, TaskRegistry},
    scheduler::{ResourceRequest, SessionLimits},
    store::{HistoryIo, HistoryWork},
    tool::ToolRegistry,
    workflow::{
        MachineIdentity, MachineRegistry, MachineStatus, MachineTransition, ResumableMachine,
        WorkflowCommand,
    },
};
use acyclic_stream::{MemoryStream, UnixMillisClock};

struct Clock;
impl UnixMillisClock for Clock {
    fn now_unix_millis(&self) -> u64 {
        1
    }
}

struct OneCommand {
    identity: MachineIdentity,
    schema: serde_json::Value,
    command: WorkflowCommand,
}
impl ResumableMachine for OneCommand {
    fn identity(&self) -> &MachineIdentity {
        &self.identity
    }
    fn state_schema(&self) -> &serde_json::Value {
        &self.schema
    }
    fn initialize(&self, input: &serde_json::Value) -> Result<serde_json::Value> {
        let _ = input;
        Ok(serde_json::Value::Bool(false))
    }
    fn transition(
        &self,
        _: &serde_json::Value,
        _: &serde_json::Value,
    ) -> Result<MachineTransition> {
        panic!("stock native fixture must use original-ledger machine callbacks")
    }
    fn supports_native_work(&self) -> bool {
        true
    }
    fn identity_with_native_work<'a>(
        &'a self,
        work: &NativeMaterializationWork,
    ) -> Result<&'a MachineIdentity> {
        work.remaining()?;
        Ok(&self.identity)
    }
    fn initialize_with_native_work(
        &self,
        input: &serde_json::Value,
        work: &NativeMaterializationWork,
    ) -> Result<(serde_json::Value, acyclic_fs::NativeAllocation)> {
        let _ = input;
        Ok((
            serde_json::Value::Bool(false),
            work.claim(
                std::mem::size_of::<serde_json::Value>() as u64,
                0,
                acyclic_fs::WorkCounters::UNCHARGED,
            )?,
        ))
    }
    fn transition_with_native_work(
        &self,
        state: &serde_json::Value,
        input: serde_json::Value,
        work: &NativeMaterializationWork,
    ) -> Result<(MachineTransition, acyclic_fs::NativeAllocation)> {
        drop(input);
        let mut history = HistoryWork::new(PreparationIo::from_native_work(work)?);
        history.retain_owned(std::mem::size_of::<MachineTransition>() as u64)?;
        let transition = if state == &serde_json::Value::Bool(true) {
            MachineTransition {
                state: serde_json::Value::Bool(true),
                commands: Vec::new(),
                status: MachineStatus::Completed {
                    value: serde_json::Value::Null,
                },
            }
        } else {
            let slot = std::mem::size_of::<WorkflowCommand>() as u64;
            history.retain_owned(slot)?;
            history.allocation_attempt(slot)?;
            let mut commands = Vec::new();
            commands.try_reserve_exact(1)?;
            if commands.capacity() > 1 {
                history.retain_owned((commands.capacity() as u64 - 1) * slot)?;
            }
            let kind = &self.command.kind;
            history.retain_owned(kind.len() as u64)?;
            history.allocation_attempt(kind.len() as u64)?;
            work.charge(acyclic_fs::WorkCounters {
                bytes_copied: kind.len() as u64,
                ..acyclic_fs::WorkCounters::UNCHARGED
            })?;
            let kind = kind.to_owned();
            let payload = self.command.payload.copy(&mut Some(&mut history))?;
            commands.push(WorkflowCommand {
                operation_id: self.command.operation_id,
                kind,
                payload,
            });
            MachineTransition {
                state: serde_json::Value::Bool(true),
                commands,
                status: MachineStatus::Suspended,
            }
        };
        let custody = history.into_custody()?;
        Ok((transition, custody))
    }
}

#[tokio::test]
async fn every_adapter_runs_a_stock_turn_on_the_issued_native_task() -> Result<()> {
    stock_turn(false).await
}

#[tokio::test]
async fn image_attach_and_paste_run_on_the_issued_native_task() -> Result<()> {
    stock_turn(true).await
}

async fn stock_turn(with_images: bool) -> Result<()> {
    for (adapter, response) in [
        (
            Adapter::Chat,
            "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n",
        ),
        (
            Adapter::Anthropic,
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
        (
            Adapter::Responses,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
        ),
        (
            Adapter::Gemini,
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"done\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":1}}\n\n",
        ),
    ] {
        let transport = Arc::new(Fixture::new(response));
        let mut provider = if adapter == Adapter::Responses {
            let mut registry = Registry::with_transport(
                Credentials::explicit(BTreeMap::from([(
                    "openai".into(),
                    "fixture-api-key".into(),
                )])),
                transport.clone(),
            );
            let catalog = tempfile::NamedTempFile::new().map_err(|_| invalid("fixture catalog"))?;
            std::fs::write(catalog.path(), include_str!("fixtures/luna-catalog.json"))
                .map_err(|_| invalid("fixture catalog"))?;
            registry.load_catalog(catalog.path())?;
            let selected = registry.resolve("openai/gpt-6-luna")?;
            assert_eq!(
                selected.context_capacity(selected.model())?.context_tokens,
                8192
            );
            selected
        } else {
            let (mut provider, _) = provider(adapter, 200);
            provider.transport = transport.clone();
            provider
        };
        let model = provider.model().clone();
        let storage_provider =
            ProviderRef::new("fixture".into(), "filesystem".into(), "2".into(), &mut None)?;
        let host = FilesystemHost::new(acyclic_fs::Fs::memory(), &storage_provider, None)?;
        let agent = AgentId::from_bytes([2; 16]);
        let volume = VolumeRef::new(
            storage_provider,
            "private".into(),
            VolumeClass::AgentPrivate,
            VolumeOwner::Agent(agent),
            &mut None,
        )?;
        host.create_volume(&volume).await?;
        let issuer = AuthorityIssuer::new(
            "fixture",
            [47; 32],
            Authority {
                kind: AggregateKind::Task,
                id: "owner".into(),
            },
        );
        let signed = issuer.issue(
            "owner",
            &Capabilities::new([
                "operation:declare".into(),
                "operation:observe".into(),
                "operation:cancel".into(),
                "task:spawn:model@1".into(),
                "model:generate".into(),
                volume.capability(VolumeOperation::Read, &mut None)?,
                volume.capability(VolumeOperation::Write, &mut None)?,
            ]),
            Some(agent),
            None,
            &mut None,
        )?;
        let scope = RuntimeScope::new(signed.capabilities().clone(), Limits::default())?;
        let work = work(&CancellationToken::new())?;
        let io = PreparationIo::from_native_work(&work)?;
        let operation = OperationId::from_bytes([1; 16]);
        let payloads = FilesystemContentPublisher::new(
            host.clone(),
            &volume,
            &issuer.verifier(&mut None)?,
            &signed,
            65_536,
            None,
        )?;
        let input = if with_images {
            use acyclic_harness::{
                client::{attach_image, paste_image},
                conversation::ContentPublisher,
                filesystem::FilesystemContentVerifier,
                model::{
                    FileProjectionPolicy, ImageDetail, ModelContentPart, NativeMediaIntent,
                    NativeMediaPolicy,
                },
                tui::{ClipboardContent, prepare_image},
            };
            use base64::Engine as _;
            let clipboard = prepare_image(2, 2, vec![255; 16]).expect("clipboard image");
            let ClipboardContent::Image { jpeg, .. } = &clipboard else {
                unreachable!()
            };
            let (selected, hold) = ContentPublisher::stage_once(
                payloads.as_ref(),
                OperationId::new(),
                "attachments/selected.jpg",
                jpeg,
                "image/jpeg",
                "selected.jpg",
                None,
            )?
            .await?;
            drop(hold);
            let reader = FilesystemContentVerifier::new(
                host.clone(),
                &issuer.verifier(&mut None)?,
                &signed,
                65_536,
                Some(&io),
            )?;
            let attached = attach_image(selected, reader.as_ref(), 4096).await?;
            let pasted =
                paste_image(payloads.as_ref(), OperationId::new(), &clipboard, 4096).await?;
            provider = provider.with_content_reader(reader);
            Arc::make_mut(&mut provider.config).images.insert(
                model.name.clone(),
                crate::ImageInput {
                    maximum_bytes: 4096,
                    maximum_total_bytes: 8192,
                    maximum_images: 4,
                    maximum_dimension: 1024,
                    maximum_tokens: 1024,
                },
            );
            transport
                .expected_images
                .lock()
                .expect("fixture image expectations")
                .push((
                    base64::engine::general_purpose::STANDARD
                        .encode(jpeg)
                        .into_bytes(),
                    2,
                ));
            let mut parts = vec![ModelContentPart::Text {
                text: "question".into(),
            }];
            for attachment in [attached, pasted] {
                parts.push(ModelContentPart::File {
                    file: attachment.file,
                    policy: FileProjectionPolicy::Native(Box::new(NativeMediaPolicy {
                        intent: NativeMediaIntent::Image {
                            detail: ImageDetail::Auto,
                        },
                        maximum_bytes: 4096,
                        maximum_work: 1024,
                        configuration: None,
                    })),
                });
            }
            ModelContent::Parts(parts)
        } else {
            ModelContent::Text("question".into())
        };
        let provider = Arc::new(provider);
        let command = ModelTaskCommand {
            input,
            selected_context: None,
            max_steps: 1,
            max_output_tokens: None,
            selected_output_volume: None,
            mailbox_operation: None,
        };
        let payload = payloads
            .stage(
                operation,
                "command",
                &serde_json::to_vec(&command).expect("command fixture"),
                None,
            )?
            .await?;
        let machine = Arc::new(OneCommand {
            identity: MachineIdentity {
                name: "model".into(),
                version: "1".into(),
                digest: [48; 32],
            },
            schema: json!({"type":"boolean"}),
            command: WorkflowCommand {
                operation_id: OperationId::from_bytes([3; 16]),
                kind: MODEL_TASK_COMMAND_KIND.into(),
                payload,
            },
        });
        let mut tasks = TaskRegistry::default();
        tasks.register(
            TaskDefinition::<serde_json::Value, serde_json::Value>::resumable(
                machine.clone(),
                json!({"type":"boolean"}),
                json!({"type":"null"}),
            )?
            .publish(&work)?,
        )?;
        let mut machines = MachineRegistry::default();
        machines.register(machine, &mut None)?;
        let runtime = FilesystemTaskRuntime::open(
            Arc::new(MemoryStream::default()),
            host.clone(),
            &volume,
            &issuer.verifier(&mut None)?,
            &signed,
            &scope,
            &tasks,
            &machines,
            &ToolRegistry::default(),
            SessionLimits {
                active_tasks: 1,
                total_tasks: 1,
                depth: 1,
                model_steps: 1,
            },
            1,
            65_536,
            Arc::new(Clock),
            None,
            &[],
            &ResumableToolRegistry::default(),
            Some(&io),
        )
        .await?;
        let definition =
            tasks.get_version::<serde_json::Value, serde_json::Value>("model", "1", None)?;
        assert!(matches!(
            runtime
                .harness()
                .admit(operation, &definition, serde_json::Value::Bool(false), None)
                .await?,
            Admission::Accepted(_)
        ));
        let mut history = HistoryWork::new(PreparationIo::from_native_work(&work)?);
        history.retain_owned(
            std::mem::size_of::<WorkPull>().max(std::mem::size_of::<TaskWorkerAttempt>()) as u64,
        )?;
        let pulled = runtime
            .task_host()
            .pull_work(
                &Worker {
                    id: "fixture".into(),
                    available: ResourceRequest::default(),
                    labels: BTreeMap::new(),
                },
                Some(operation),
                &mut Some(&mut history),
            )
            .await?;
        let WorkPull::Claimed(lease) = pulled else {
            panic!("native lease")
        };
        let recovery = history.into_custody()?;
        let context = ContextPipeline::default();
        let policy = CompactionPolicy::Disabled;
        let commands =
            runtime
                .commands()
                .with_model(&model, provider, &context, &policy, Some(&io))?;
        let (attempt, custody) = runtime
            .resume_task_with_native_work(lease, recovery, &commands, 2, &work)
            .await?;
        if let TaskWorkerAttempt::Unresolved { error, .. } = attempt {
            return Err(error);
        }
        assert!(
            matches!(
                attempt,
                TaskWorkerAttempt::Progress(TaskWorkerOutcome::Completed { .. })
            ),
            "{adapter:?}"
        );
        assert!(custody.work().shares_allocation_context(&work));
        assert_eq!(transport.sent.load(Ordering::SeqCst), 1, "{adapter:?}");
    }
    Ok(())
}

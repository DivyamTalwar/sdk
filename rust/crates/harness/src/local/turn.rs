use super::*;
use crate::{executor::TurnOutput, runtime::TaskStateProvider};

impl LocalHarness {
    /// Stock command bindings for a service-owned fenced worker.
    pub fn commands(
        &self,
    ) -> Result<
        crate::filesystem::FilesystemTaskCommands<
            '_,
            LocalStream,
            LocalAuthorityBackend,
            LocalObjectBackend,
        >,
    > {
        self.runtime.commands().with_model(
            &self.model,
            self.provider.clone(),
            &self.context,
            &self.compaction,
            None,
        )
    }
    /// Stages and admits a resumable stock turn under a stable operation identity.
    pub async fn submit(
        &self,
        operation: OperationId,
        prompt: &str,
    ) -> Result<Admission<RuntimeTask<TurnOutput>>> {
        if prompt.is_empty() || prompt.len() as u64 > self.limits.file_bytes {
            return Err(Error::Invalid(
                "local prompt is empty or exceeds the file limit".into(),
            ));
        }
        let publisher = FilesystemContentPublisher::new(
            self.files.clone(),
            &self.journal,
            &self.verifier,
            &self.signed,
            self.limits.file_bytes,
            None,
        )?;
        let model = crate::filesystem::ModelTaskCommand {
            input: ModelContent::Text(prompt.into()),
            selected_context: None,
            max_steps: u32::try_from(self.limits.model_steps)
                .map_err(|_| Error::Invalid("model step limit exceeds u32".into()))?,
            max_output_tokens: None,
            mailbox_operation: None,
            selected_output_volume: Some(self.scratch.clone()),
        };
        let bytes = serde_json::to_vec(&model).map_err(storage)?;
        let payload = publisher
            .stage_once(
                operation,
                &format!("inputs/{operation}.json"),
                &bytes,
                "application/json",
                "model input",
                None,
            )?
            .await?
            .0;
        let model = WorkflowCommand {
            operation_id: operation,
            kind: crate::filesystem::MODEL_TASK_COMMAND_KIND.into(),
            payload,
        };
        let receive_operation = receive_operation(operation);
        let payload = publisher
            .stage_once(
                receive_operation,
                &format!("inputs/{operation}.receive.json"),
                b"{\"after\":0}",
                "application/json",
                "mailbox receive",
                None,
            )?
            .await?
            .0;
        let receive = WorkflowCommand {
            operation_id: receive_operation,
            kind: crate::filesystem::MAIL_RECEIVE_TASK_COMMAND_KIND.into(),
            payload,
        };
        let definition = self
            .runtime
            .harness()
            .task::<serde_json::Value, TurnOutput>("acyclic.stock_turn@1")?;
        self.runtime
            .harness()
            .admit(
                operation,
                &definition,
                serde_json::json!({"receive":receive,"model":model}),
                None,
            )
            .await
    }

    /// Runs a prompt through durable admission, the fenced worker and stock executor.
    pub async fn run(&self, prompt: &str) -> Result<TurnOutput> {
        let operation = OperationId::new();
        match Box::pin(self.submit(operation, prompt)).await? {
            Admission::Accepted(RuntimeTask::Durable { task_id, .. }) => {
                Box::pin(self.resume(task_id)).await
            }
            Admission::Accepted(RuntimeTask::Live(_)) => Err(Error::Invalid(
                "local stock turn was not durably admitted".into(),
            )),
            Admission::Indeterminate { .. } => Err(Error::Indeterminate(operation)),
            Admission::Rejected { reason } => Err(Error::Conflict(reason)),
        }
    }

    /// Drives an admitted stock task, including a task admitted before reopen.
    pub async fn resume(&self, task: TaskId) -> Result<TurnOutput> {
        let worker = crate::distributed::Worker {
            id: format!("local-{}", uuid::Uuid::new_v4()),
            available: Default::default(),
            labels: Default::default(),
        };
        let commands = self.commands()?;
        let mut attempt = Box::pin(
            self.runtime
                .run_operation(&worker, task, &commands, 32, None),
        )
        .await?;
        loop {
            match attempt {
                Some(crate::filesystem::TaskWorkerAttempt::Unresolved { error, .. }) => {
                    return Err(error);
                }
                Some(crate::filesystem::TaskWorkerAttempt::Progress(
                    crate::filesystem::TaskWorkerOutcome::Yielded { lease },
                )) => {
                    attempt =
                        Some(Box::pin(self.runtime.resume_task(lease, &commands, 32, None)).await);
                }
                _ => break,
            }
        }
        let definition = self
            .runtime
            .harness()
            .task::<serde_json::Value, TurnOutput>("acyclic.stock_turn@1")?;
        let handle = self.runtime.harness().attach(task, &definition).await?;
        if self.runtime.task_host().outcome(task).await?.is_none() {
            return Err(Error::Indeterminate(OperationId::from_bytes(
                task.into_bytes(),
            )));
        }
        match handle.result().await? {
            crate::Outcome::Succeeded(output) => Ok(output),
            crate::Outcome::Failed { message } => Err(Error::Conflict(message)),
            crate::Outcome::Cancelled => Err(Error::InteractionRejected(
                crate::InteractionRejection::Cancelled,
            )),
            crate::Outcome::Indeterminate { operation_id } => {
                Err(Error::Indeterminate(operation_id))
            }
        }
    }
}

fn receive_operation(operation: OperationId) -> OperationId {
    let mut hash = blake3::Hasher::new();
    hash.update(b"acyclic.local.mail-receive/v1\0");
    hash.update(&operation.into_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    OperationId::from_bytes(bytes)
}

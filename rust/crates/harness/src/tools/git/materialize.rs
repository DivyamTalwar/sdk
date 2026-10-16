use super::*;

/// Publishes git facade mutations into the ordinary checkout.
pub(crate) struct RootMaterializingGitExecutor<'a, F> {
    /// Managed workspace executor.
    pub inner: WorkspaceGitExecutor<'a>,
    /// Ordinary checkout path.
    pub root: &'a Path,
    /// Pinned physical checkout identity.
    pub source_root_identity: acyclic_fs::NativeRootIdentity,
    /// Revalidates the admitted task and its publication permit before each effect.
    pub before_effect: F,
}

impl<F, Fut> RootMaterializingGitExecutor<'_, F>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), Error>> + Send,
{
    /// Publishes a generation transition into the checkout.
    pub async fn materialize(
        &self,
        operation_id: OperationId,
        from: LocalGeneration,
    ) -> Result<(), Error> {
        let target = self
            .inner
            .switched
            .lock()
            .map_err(|_| WorkspaceGitExecutor::error("Git workspace switch lock is unavailable"))?
            .as_ref()
            .map_or_else(
                || self.inner.current.clone(),
                |lazy| lazy.workspace().clone(),
            );
        let to_generation = target
            .head(WorkBudget::UNBOUNDED, &CancellationToken::new())
            .await
            .map(|receipt| receipt.value)
            .map_err(|failure| failure.error)
            .map_err(display)?;
        publish_checkout_transition(
            self.root,
            self.source_root_identity,
            self.inner.store,
            operation_id,
            from,
            to_generation,
            &|| self.validate(),
        )
        .await
    }
}

pub(crate) async fn publish_checkout_transition<F, Fut>(
    root: &Path,
    source_root_identity: acyclic_fs::NativeRootIdentity,
    store: &LocalCoreStateStore,
    operation_id: OperationId,
    from: LocalGeneration,
    to_generation: LocalGeneration,
    before_effect: &F,
) -> Result<(), Error>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), Error>> + Send,
{
    before_effect().await?;
    let to = to_generation.id();
    if from.id() == to {
        return Ok(());
    }
    let operation_directory = NativeDirectory(root)
        .publication_directory(operation_id)
        .map_err(Error::Storage)?;
    let parent = operation_directory
        .parent()
        .ok_or_else(|| Error::Storage("staging parent is absent".into()))?;
    std::fs::create_dir_all(parent).map_err(display)?;
    let cancellation = CancellationToken::new();
    let work = acyclic_fs::NativeMaterializationWork::new(WorkBudget::UNBOUNDED, &cancellation)
        .map_err(|(cause, _)| display(cause))?;
    let actual =
        acyclic_fs::validate_native_preparation_location(root, &operation_directory, &work)
            .map_err(display)?;
    if actual != source_root_identity {
        return Err(Error::Storage(
            "checkout root changed before publication".into(),
        ));
    }
    let options = MaterializeOptions {
        destination: operation_directory.join("target"),
        expected_root_identity: None,
        maximum_directory_entries: 4_096,
        maximum_extent_spans: 65_536,
        transfer_bytes: 8 * 1024 * 1024,
    };
    let cancellation = CancellationToken::new();
    acyclic_fs::publish_native_generation_transition(
        &from,
        &to_generation,
        store,
        acyclic_fs::NativeWorkspacePublication {
            root,
            source_root_identity,
            operation_directory: &operation_directory,
            operation_id,
            from: from.id(),
            to,
            excluded_names: &[".git", ".system"],
            options: &options,
            budget: WorkBudget::UNBOUNDED,
            cancellation: &cancellation,
        },
        acyclic_fs::NativeMaterializationWork::new(WorkBudget::UNBOUNDED, &cancellation)
            .map_err(|(cause, _)| display(cause))?,
        || async {
            before_effect().await.map_err(|error| {
                acyclic_fs::NativeWorkspacePublicationError::Native(
                    acyclic_fs::NativeTreeMaterializationError::Io(std::io::Error::other(error)),
                )
            })
        },
    )
    .await
    .map_err(|(cause, _)| display(cause))?;
    store
        .remove_materialization_async(operation_id)
        .await
        .map_err(display)?;
    NativeDirectory(
        &NativeDirectory(root)
            .publication_root()
            .map_err(Error::Storage)?,
    )
    .remove(&operation_directory)
    .map_err(display)?;
    Ok(())
}

impl<F, Fut> GitFilesystemExecutor for RootMaterializingGitExecutor<'_, F>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), Error>> + Send,
{
    type Error = Error;

    async fn validate(&self) -> Result<(), Self::Error> {
        (self.before_effect)().await?;
        self.inner.validate().await
    }

    async fn execute(
        &self,
        operation_id: OperationId,
        action: &GitFilesystemAction,
    ) -> Result<GitFilesystemResult, Self::Error> {
        let from = self
            .inner
            .current
            .head(
                acyclic_fs::WorkBudget::UNBOUNDED,
                &acyclic_fs::CancellationToken::new(),
            )
            .await
            .map(|receipt| receipt.value)
            .map_err(|failure| failure.error)
            .map_err(display)?;
        let result = self.inner.execute(operation_id, action).await;
        // Conflict projection is an intentional state transition: the join
        // executor leaves the Git transition pending and returns an error so
        // the caller can present the typed conflicts. Materialize that new
        // generation before propagating the error, otherwise the checkout
        // would never expose the files the user must edit.
        let materialized = self.materialize(operation_id, from).await;
        match (result, materialized) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), Ok(())) | (_, Err(error)) => Err(error),
        }
    }
}

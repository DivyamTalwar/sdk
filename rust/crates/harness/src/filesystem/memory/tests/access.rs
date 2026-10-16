use super::*;
use acyclic_fs::access::{Action as FsAction, Effect, Policy};
use acyclic_fs::{
    CancellationToken, WorkBudget,
    model::{CheckoutMode, GenerationSelector},
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn cached_bytes_never_bypass_policy_revisions(revisions in prop::collection::vec(any::<bool>(), 1..5)) {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
            let fs = Fs::memory();
            let storage = MemoryHarnessStorage::from_providers(
                AgentId::new(), 4096, fs.clone(), Arc::new(MemoryStream::default()),
            ).await?;
            let file = storage.stage(OperationId::new(), "secrets/key", b"cached", "text/plain", "key").await?;
            assert!(storage.memory_store.lock().unwrap().read(&file).is_ok());
            let cancel = CancellationToken::new();
            let workspace = fs.open_workspace(storage.volume.storage_name(&mut None)?, WorkBudget::UNBOUNDED, &cancel)
                .await.map_err(|failure| failure.error)?.value;
            for (index, denied) in revisions.into_iter().enumerate() {
                let mut policy: Policy = serde_json::from_slice(super::super::super::access::INITIAL_POLICY)
                    .map_err(|error| Error::Invalid(error.to_string()))?;
                policy.version = index as u64 + 2;
                if denied {
                    let mut deny = policy.layers[0][0].clone();
                    deny.id = "hide-cached-bytes".into();
                    deny.effect = Effect::Deny;
                    deny.actions = [FsAction::Read].into_iter().collect();
                    deny.resource.path = "secrets/**".into();
                    policy.layers[0].push(deny);
                }
                let mut checkout = workspace.checkout(GenerationSelector::Head, CheckoutMode::tracking_transaction(),
                    WorkBudget::UNBOUNDED, &cancel).await.map_err(|failure| failure.error)?.value;
                checkout.write_access_policy(&serde_json::to_vec(&policy).map_err(|error| Error::Invalid(error.to_string()))?,
                    acyclic_fs::NamedAttributeWriteMode::Replace, WorkBudget::UNBOUNDED, &cancel)
                    .await.map_err(|failure| failure.error)?;
                checkout.commit::<acyclic_fs::FsError, _, _>(
                    acyclic_fs::OperationId::new(), None, None, None, acyclic_fs::PublicationPermit::Unrestricted,
                    WorkBudget::UNBOUNDED, &cancel,
                    |_| std::future::ready(Ok(acyclic_fs::OperationReceipt {
                        value: (), work: acyclic_fs::WorkCounters::UNCHARGED,
                    })),
                ).await.map_err(|failure| failure.error)?;
                // Keep proving that the old shortcut still has the bytes: a
                // denial must come from the current volume policy, not eviction.
                assert!(storage.memory_store.lock().unwrap().read(&file).is_ok());
                let read = storage.read(&file).await;
                assert_eq!(read.is_ok(), !denied, "{read:?}");
                if let Ok(bytes) = read { assert_eq!(bytes.as_ref(), b"cached"); }
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        }).map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

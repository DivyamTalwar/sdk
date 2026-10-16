use super::*;
use crate::{Checkout, CheckoutMode, GenerationSelector, NamedAttributeWriteMode};
use proptest::prelude::*;

type MemoryCheckout = Checkout<crate::MemoryAuthorityBackend, crate::MemoryObjectBackend>;
type MemoryWorkspace = Workspace<crate::MemoryAuthorityBackend, crate::MemoryObjectBackend>;

async fn current(
    workspace: &MemoryWorkspace,
    writable: bool,
) -> Result<MemoryCheckout, Box<dyn Error>> {
    Ok(unbounded!(
        workspace,
        checkout(
            GenerationSelector::Head,
            if writable {
                CheckoutMode::tracking_transaction()
            } else {
                CheckoutMode::read_only_pinned()
            }
        )
    )?)
}

async fn recorded(checkout: &mut MemoryCheckout) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    Ok(unbounded!(checkout, read_access_policy())?.map(|chunk| chunk.bytes.to_vec()))
}

async fn publish(checkout: &mut MemoryCheckout) -> Result<(), Box<dyn Error>> {
    let result = checkout
        .commit(
            crate::OperationId::new(),
            None,
            None,
            None,
            crate::PublicationPermit::Unrestricted,
            WorkBudget::UNBOUNDED,
            &CancellationToken::new(),
            allow::<FsError>,
        )
        .await?
        .value;
    assert!(matches!(
        result,
        crate::kernel::LivePublicationObservation::Committed { .. }
            | crate::kernel::LivePublicationObservation::AlreadyCommitted { .. }
    ));
    Ok(())
}

// Each published root has its own metadata; staging never changes another view,
// and a fork inherits policy even when every namespace path is filtered out.
async fn policy_history(revisions: Vec<Vec<u8>>) -> Result<(), Box<dyn Error>> {
    let fs = Fs::memory();
    let workspace = unbounded!(fs, create_workspace("policy-parent"))?;
    let mut writer = current(&workspace, true).await?;
    assert_eq!(recorded(&mut writer).await?, None);
    let mut snapshots = Vec::new();
    let mut previous = None;
    for (index, revision) in revisions.iter().enumerate() {
        snapshots.push((current(&workspace, false).await?, previous.clone()));
        unbounded!(
            writer,
            write_access_policy(
                revision,
                if index == 0 {
                    NamedAttributeWriteMode::Create
                } else {
                    NamedAttributeWriteMode::Replace
                }
            )
        )?;
        assert_eq!(
            recorded(&mut writer).await?.as_deref(),
            Some(revision.as_slice())
        );
        let mut before = current(&workspace, false).await?;
        assert_eq!(recorded(&mut before).await?, previous);
        publish(&mut writer).await?;
        let mut after = current(&workspace, false).await?;
        assert_eq!(
            recorded(&mut after).await?.as_deref(),
            Some(revision.as_slice())
        );
        previous = Some(revision.clone());
    }
    for (mut snapshot, expected) in snapshots {
        assert_eq!(recorded(&mut snapshot).await?, expected);
    }
    let head = unbounded!(workspace, head())?;
    for (name, paths) in [("policy-full", None), ("policy-empty", Some(Vec::new()))] {
        let options = ForkOptions::from_generation(head.clone(), IdempotencyKey::new());
        let options = match paths {
            None => options,
            Some(paths) => options.inherit_paths(paths),
        };
        let fork = fork_workspace(&workspace, name, options).await?;
        let mut child = current(&fork, false).await?;
        assert_eq!(recorded(&mut child).await?, revisions.last().cloned());
    }
    Ok(())
}

#[tokio::test]
async fn access_policy_storage_preserves_pins_and_filtered_forks() -> Result<(), Box<dyn Error>> {
    policy_history(vec![b"first".to_vec(), b"second".to_vec()]).await
}

#[tokio::test]
async fn access_policy_storage_failed_preconditions_leave_the_record_unchanged()
-> Result<(), Box<dyn Error>> {
    let fs = Fs::memory();
    let workspace = unbounded!(fs, create_workspace("policy-preconditions"))?;
    let mut checkout = current(&workspace, true).await?;
    assert!(
        unbounded!(
            checkout,
            write_access_policy(b"absent", NamedAttributeWriteMode::Replace)
        )
        .is_err()
    );
    assert_eq!(recorded(&mut checkout).await?, None);
    unbounded!(
        checkout,
        write_access_policy(b"original", NamedAttributeWriteMode::Create)
    )?;
    assert!(
        unbounded!(
            checkout,
            write_access_policy(b"replacement", NamedAttributeWriteMode::Create)
        )
        .is_err()
    );
    assert_eq!(
        recorded(&mut checkout).await?.as_deref(),
        Some(b"original".as_slice())
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn access_policy_storage_revisions_are_isolated(
        revisions in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..96), 1..5),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        runtime.block_on(policy_history(revisions))
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

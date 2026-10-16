//! Models the production transaction admission and authority CAS together.
use super::*;
use crate::{Fs, WorkCounters};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

type ModelTransaction = Transaction<crate::MemoryAuthorityStore, crate::MemoryObjectStore>;

fn ready<F: Future>(create: impl FnOnce() -> F) -> F::Output {
    // Build SDK futures in the same short heap frame used by native callbacks.
    // Keeping them in this driver would exhaust Loom's coroutine stack before
    // the modeled authority locks can run.
    match crate::heap_future::in_heap(create)
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the in-process model backend must not yield"),
    }
}

fn commit(transaction: &mut ModelTransaction) -> bool {
    let cancel = CancellationToken::new();
    matches!(
        ready(|| transaction.commit::<WorkspaceError, _, _>(
            None,
            crate::PublicationPermit::Unrestricted,
            WorkBudget::UNBOUNDED,
            &cancel,
            |_| std::future::ready(Ok(crate::OperationReceipt {
                value: (),
                work: WorkCounters::UNCHARGED,
            })),
        ))
        .unwrap()
        .value,
        TransactionCommit::Committed(_)
    )
}

fn model(check_base: bool) {
    loom::model(move || {
        // Only authority state is shared mutable publication state. Its real
        // backend mutex uses crate::sync; immutable objects keep their ordinary
        // store. No test lock wraps an uninstrumented publication algorithm.
        let fs = Fs::new(
            crate::MemoryAuthorityStore::default(),
            crate::MemoryObjectStore::default(),
            crate::EmbeddedCapabilities::MEMORY,
        );
        let cancel = CancellationToken::new();
        let workspace =
            ready(|| fs.create_workspace("access-write", WorkBudget::UNBOUNDED, &cancel))
                .unwrap()
                .value;
        let authorized = ready(|| workspace.head(WorkBudget::UNBOUNDED, &cancel))
            .unwrap()
            .value;
        let mut revocation = ready(|| {
            workspace.begin_transaction(
                IdempotencyKey::from_bytes([1; 16]),
                WorkBudget::UNBOUNDED,
                &cancel,
            )
        })
        .unwrap()
        .value;
        ready(|| {
            revocation.checkout.write_access_policy(
                br#"{"schema_version":1,"version":2,"layers":[]}"#,
                crate::NamedAttributeWriteMode::Create,
                WorkBudget::UNBOUNDED,
                &cancel,
            )
        })
        .unwrap();
        let revoker = loom::thread::spawn(move || commit(&mut revocation));
        let key = IdempotencyKey::from_bytes([2; 16]);
        let mut write = if check_base {
            ready(|| {
                workspace.begin_transaction_at(&authorized, key, WorkBudget::UNBOUNDED, &cancel)
            })
            .unwrap()
            .value
        } else {
            // Negative control: the deleted pinned constructor paired Exact's
            // old root with the newly observed head and skipped the stale flag.
            let checkout = ready(|| {
                workspace.checkout(
                    GenerationSelector::Exact(authorized.id()),
                    CheckoutMode::tracking_transaction(),
                    WorkBudget::UNBOUNDED,
                    &cancel,
                )
            })
            .unwrap()
            .value;
            Transaction::from_checkout(&workspace, checkout, key, false)
        };
        ready(|| write.write_text("/target", "new")).unwrap();
        let published = commit(&mut write);
        let revoked = revoker
            .join()
            .expect("revocation completes without deadlock");
        assert!(
            !(published && revoked),
            "write crossed its authorization generation"
        );
    });
}

#[test]
fn access_write_model_fences_revocation_before_and_after_transaction_admission() {
    model(true);
}

#[test]
#[should_panic(expected = "write crossed its authorization generation")]
fn access_write_model_detects_an_exact_root_bound_to_a_newer_head() {
    model(false);
}

//! Models the production transaction admission and authority CAS together.
use super::*;
use crate::{Fs, WorkCounters};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

type ModelTransaction = Transaction<crate::MemoryAuthorityStore, crate::MemoryObjectStore>;

fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future)
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the in-process model backend must not yield"),
    }
}

fn commit(transaction: &mut ModelTransaction) -> bool {
    matches!(
        ready(transaction.commit::<WorkspaceError, _, _>(
            None,
            crate::PublicationPermit::Unrestricted,
            WorkBudget::UNBOUNDED,
            &CancellationToken::new(),
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
        let workspace = ready(fs.create_workspace("access-write", WorkBudget::UNBOUNDED, &cancel))
            .unwrap()
            .value;
        let authorized = ready(workspace.head(WorkBudget::UNBOUNDED, &cancel))
            .unwrap()
            .value
            .id();
        let mut revocation = ready(workspace.begin_transaction(
            IdempotencyKey::new(),
            WorkBudget::UNBOUNDED,
            &cancel,
        ))
        .unwrap()
        .value;
        ready(revocation.checkout.write_access_policy(
            br#"{"schema_version":1,"version":2,"layers":[]}"#,
            crate::NamedAttributeWriteMode::Create,
            WorkBudget::UNBOUNDED,
            &cancel,
        ))
        .unwrap();
        let revoker = loom::thread::spawn(move || commit(&mut revocation));
        let mut write = ready(workspace.begin_transaction(
            IdempotencyKey::new(),
            WorkBudget::UNBOUNDED,
            &cancel,
        ))
        .unwrap()
        .value;
        let published = if check_base && !write.is_based_on(authorized) {
            false
        } else {
            ready(write.write_text("/target", "new")).unwrap();
            commit(&mut write)
        };
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
fn access_write_model_detects_missing_generation_check() {
    model(false);
}

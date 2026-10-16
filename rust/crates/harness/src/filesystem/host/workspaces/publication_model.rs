//! Explore the adapter's real retry path at atomic provider publication calls.
//! This models the caller protocol, not the filesystem provider's own locks.
use std::cell::Cell;

loom::thread_local! {static SKIP:Cell<bool> = Cell::new(false);}
pub(super) async fn before_commit() {
    loom::thread::yield_now();
}
pub(super) fn skip_retry() -> bool {
    SKIP.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    fn model(broken: bool) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let provider = ProviderRef::new(
            "publication-model".into(),
            "filesystem".into(),
            "2".into(),
            &mut None,
        )
        .unwrap();
        let filesystem = Fs::memory();
        loom::future::block_on(Box::pin(filesystem.create_workspace(
            "shared",
            acyclic_fs::WorkBudget::UNBOUNDED,
            &acyclic_fs::CancellationToken::new(),
        )))
        .unwrap();
        let host = FilesystemHost::new(filesystem, &provider, None).unwrap();
        let workspace = workspace_ref(provider, "shared").unwrap();
        let writers: Vec<_> = ["left", "right"]
            .into_iter()
            .map(|name| {
                let host = host.clone();
                let workspace = workspace.clone();
                loom::thread::spawn(move || {
                    SKIP.with(|mode| mode.set(broken));
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    let _entered = runtime.enter();
                    let mutations = [WorkspaceMutation::PutFile {
                        path: format!("/{name}"),
                        bytes: name.as_bytes().to_vec().into(),
                    }];
                    loom::future::block_on(Box::pin(host.apply(
                        &workspace,
                        None,
                        &mutations,
                        &IdempotencyKey::new(name).unwrap(),
                    )))
                })
            })
            .collect();
        let outcomes: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();
        let succeeded = outcomes.iter().all(Result::is_ok);
        let mut preserved = true;
        if succeeded {
            for name in ["left", "right"] {
                preserved &= loom::future::block_on(Box::pin(host.read(
                    &workspace,
                    None,
                    &format!("/{name}"),
                    64,
                    None,
                )))
                .unwrap()
                .as_ref()
                    == name.as_bytes();
            }
        }
        drop(host);
        drop(workspace);
        assert!(succeeded, "independent publication lost: {outcomes:?}");
        assert!(preserved, "publication lost an earlier writer's file");
    }

    #[test]
    fn independent_publications_reapply_without_losing_either_file() {
        loom::model(|| model(false));
    }

    #[test]
    #[should_panic(expected = "independent publication lost")]
    fn negative_control_detects_rejecting_an_unconditional_stale_candidate() {
        loom::model(|| model(true));
    }
}

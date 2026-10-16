#[tokio::test]
async fn parent_observed_during_reconcile_is_not_lost() {
    let workspace = WorkspaceId::derive(
        [8; 16],
        &crate::WorkspaceName::new(
            "agent",
            &crate::WorkBudget::UNBOUNDED,
            &crate::CancellationToken::new(),
        )
        .expect("valid test workspace")
        .value,
        &crate::WorkBudget::UNBOUNDED,
        &crate::CancellationToken::new(),
    )
    .expect("unbounded workspace identity")
    .value;
    let coordinator = OperationWindowCoordinator::new(MemoryOperationWindowStore::new());
    let lease = coordinator
        .begin(workspace, generation(1), "tool", 10, 100)
        .await
        .expect("lease");
    let OperationWindowFinish::Reconcile(reconcile) =
        coordinator.finish(&lease, 20).await.expect("finish")
    else {
        panic!("final close must claim reconciliation");
    };
    coordinator
        .observe_parent(workspace, generation(2))
        .await
        .expect("observe during reconcile");
    coordinator
        .complete_reconcile(workspace, reconcile.ticket)
        .await
        .expect("complete first reconcile");
    assert!(matches!(
        coordinator.inspect(workspace).await.expect("inspect").phase,
        OperationWindowPhase::Reconciling {
            pending_parent: Some(parent),
            ..
        } if parent == generation(2)
    ));
}

#[tokio::test]
async fn expired_lease_is_fenced_and_recovery_ticket_is_claimable() {
    let workspace = WorkspaceId::derive(
        [9; 16],
        &crate::WorkspaceName::new(
            "expired",
            &crate::WorkBudget::UNBOUNDED,
            &crate::CancellationToken::new(),
        )
        .expect("valid test workspace")
        .value,
        &crate::WorkBudget::UNBOUNDED,
        &crate::CancellationToken::new(),
    )
    .expect("unbounded workspace identity")
    .value;
    let coordinator = OperationWindowCoordinator::new(MemoryOperationWindowStore::new());
    let lease = coordinator
        .begin(workspace, generation(1), "tool", 10, 20)
        .await
        .expect("lease");
    assert_eq!(
        coordinator.finish(&lease, 20).await.expect("expired close"),
        OperationWindowFinish::AlreadyClosed
    );
    let claimed = coordinator
        .claim_reconcile(workspace, 20)
        .await
        .expect("claim")
        .expect("recovery ticket");
    assert_eq!(claimed.pinned_parent, generation(1));
    assert!(matches!(
        coordinator
            .begin(workspace, generation(1), "late", 21, 30)
            .await,
        Err(OperationWindowError::Reconciling)
    ));
}

#[tokio::test]
async fn expired_close_is_persistently_fenced_while_another_lease_remains() {
    let workspace = WorkspaceId::derive(
        [10; 16],
        &crate::WorkspaceName::new(
            "overlap-expiry",
            &crate::WorkBudget::UNBOUNDED,
            &crate::CancellationToken::new(),
        )
        .expect("valid test workspace")
        .value,
        &crate::WorkBudget::UNBOUNDED,
        &crate::CancellationToken::new(),
    )
    .expect("unbounded workspace identity")
    .value;
    let coordinator = OperationWindowCoordinator::new(MemoryOperationWindowStore::new());
    let expired = coordinator
        .begin(workspace, generation(1), "expired", 10, 20)
        .await
        .expect("expired lease");
    let active = coordinator
        .begin(workspace, generation(1), "active", 11, 40)
        .await
        .expect("active lease");

    assert_eq!(
        coordinator
            .finish(&expired, 20)
            .await
            .expect("expired close"),
        OperationWindowFinish::AlreadyClosed
    );
    let snapshot = coordinator.inspect(workspace).await.expect("inspect");
    assert!(matches!(
        snapshot.phase,
        OperationWindowPhase::Active { ref leases, .. }
            if leases.len() == 1 && leases.contains_key(&active.lease_id)
    ));
    assert!(matches!(
        coordinator.finish(&active, 22).await.expect("active close"),
        OperationWindowFinish::Reconcile(_)
    ));
}

#[tokio::test]
async fn duplicate_close_persists_expiry_of_the_remaining_lease() {
    let workspace = WorkspaceId::derive(
        [11; 16],
        &crate::WorkspaceName::new(
            "duplicate-expiry",
            &crate::WorkBudget::UNBOUNDED,
            &crate::CancellationToken::new(),
        )
        .expect("valid test workspace")
        .value,
        &crate::WorkBudget::UNBOUNDED,
        &crate::CancellationToken::new(),
    )
    .expect("unbounded workspace identity")
    .value;
    let coordinator = OperationWindowCoordinator::new(MemoryOperationWindowStore::new());
    let first = coordinator
        .begin(workspace, generation(1), "first", 1, 10)
        .await
        .expect("first lease");
    coordinator
        .begin(workspace, generation(1), "second", 2, 40)
        .await
        .expect("second lease");
    assert_eq!(
        coordinator.finish(&first, 5).await.expect("first close"),
        OperationWindowFinish::StillActive { remaining: 1 }
    );
    assert_eq!(
        coordinator
            .finish(&first, 50)
            .await
            .expect("duplicate close"),
        OperationWindowFinish::AlreadyClosed
    );
    assert!(matches!(
        coordinator.inspect(workspace).await.expect("inspect").phase,
        OperationWindowPhase::Reconciling { .. }
    ));
}

#[tokio::test]
async fn stream_lease_gate_fences_one_writer_without_fencing_its_peer() {
    let clock = Arc::new(TestClock::default());
    clock.0.store(10, Ordering::SeqCst);
    let stream = Arc::new(acyclic_stream::MemoryStream::new_with_clock(
        acyclic_stream::MemoryLimits::default(),
        clock,
    ));
    let authority = StreamAuthorityStore::new(Arc::clone(&stream));
    let windows =
        OperationWindowCoordinator::new(StreamOperationWindowStore::new(Arc::clone(&stream)));
    let workspace = WorkspaceId::from_bytes([61; 16]);
    let authority_id = crate::kernel::volume_authority_id(workspace.volume_id());
    let unrelated_authority = AuthorityId::from_bytes([62; 16]);
    let cancellation = CancellationToken::new();
    let mut authority_work = crate::AuthorityWork::new(WorkBudget::UNBOUNDED, &cancellation);
    AsyncAuthorityStore::create_authority(
        &authority,
        unrelated_authority,
        Epoch::GENESIS,
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("create unrelated authority");
    AsyncAuthorityStore::create_authority(
        &authority,
        authority_id,
        Epoch::GENESIS,
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("create authority");
    let first = windows
        .begin(workspace, generation(1), "first", 1, 100)
        .await
        .expect("first lease");
    let second = windows
        .begin(workspace, generation(1), "second", 2, 100)
        .await
        .expect("second lease");
    assert_eq!(
        windows.finish(&first, 3).await.expect("close first"),
        OperationWindowFinish::StillActive { remaining: 1 }
    );

    let head = AsyncAuthorityStore::head(&authority, authority_id, &mut authority_work)
        .await
        .expect("authority head");
    let rejected = AsyncAuthorityStore::compare_and_append(
        &authority,
        crate::GuardedAppend {
            authority_id,
            epoch: head.epoch,
            expected: head,
            commit: ProposedCommit {
                operation_id: OperationId::from_bytes([71; 16]),
                fingerprint: Digest::from_bytes([72; 32]),
                payload: Bytes::from_static(b"first"),
            },
            permit: first.publication_permit(),
        },
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("guarded rejection");
    assert!(matches!(rejected, crate::AppendOutcome::Fenced { .. }));

    let committed = AsyncAuthorityStore::compare_and_append(
        &authority,
        crate::GuardedAppend {
            authority_id,
            epoch: head.epoch,
            expected: head,
            commit: ProposedCommit {
                operation_id: OperationId::from_bytes([73; 16]),
                fingerprint: Digest::from_bytes([74; 32]),
                payload: Bytes::from_static(b"second"),
            },
            permit: second.publication_permit(),
        },
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("guarded append");
    assert!(matches!(committed, crate::AppendOutcome::Committed(_)));

    let unrelated_head =
        AsyncAuthorityStore::head(&authority, unrelated_authority, &mut authority_work)
            .await
            .expect("unrelated authority head");
    let rejected = AsyncAuthorityStore::compare_and_append(
        &authority,
        crate::GuardedAppend {
            authority_id: unrelated_authority,
            epoch: unrelated_head.epoch,
            expected: unrelated_head,
            commit: ProposedCommit {
                operation_id: OperationId::from_bytes([75; 16]),
                fingerprint: Digest::from_bytes([76; 32]),
                payload: Bytes::from_static(b"cross-authority"),
            },
            permit: second.publication_permit(),
        },
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("cross-authority lease is a semantic fence");
    assert!(matches!(rejected, crate::AppendOutcome::Fenced { .. }));
    assert_eq!(
        AsyncAuthorityStore::head(&authority, unrelated_authority, &mut authority_work,)
            .await
            .expect("unrelated authority remains unchanged"),
        unrelated_head
    );
}

#[tokio::test]
async fn provider_deadline_fences_without_a_separate_expiry_sweep() {
    let clock = Arc::new(TestClock::default());
    clock.0.store(10, Ordering::SeqCst);
    let stream = Arc::new(acyclic_stream::MemoryStream::new_with_clock(
        acyclic_stream::MemoryLimits::default(),
        clock.clone(),
    ));
    let authority = StreamAuthorityStore::new(Arc::clone(&stream));
    let windows =
        OperationWindowCoordinator::new(StreamOperationWindowStore::new(Arc::clone(&stream)));
    let workspace = WorkspaceId::from_bytes([81; 16]);
    let authority_id = crate::kernel::volume_authority_id(workspace.volume_id());
    let cancellation = CancellationToken::new();
    let mut authority_work = crate::AuthorityWork::new(WorkBudget::UNBOUNDED, &cancellation);
    AsyncAuthorityStore::create_authority(
        &authority,
        authority_id,
        Epoch::GENESIS,
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("create authority");
    let lease = windows
        .begin(workspace, generation(1), "tool", 10, 20)
        .await
        .expect("lease");
    let head = AsyncAuthorityStore::head(&authority, authority_id, &mut authority_work)
        .await
        .expect("authority head");
    clock.0.store(20, Ordering::SeqCst);
    let outcome = AsyncAuthorityStore::compare_and_append(
        &authority,
        crate::GuardedAppend {
            authority_id,
            epoch: head.epoch,
            expected: head,
            commit: ProposedCommit {
                operation_id: OperationId::from_bytes([83; 16]),
                fingerprint: Digest::from_bytes([84; 32]),
                payload: Bytes::from_static(b"expired"),
            },
            permit: lease.publication_permit(),
        },
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("deadline is a semantic fence");
    assert!(matches!(outcome, crate::AppendOutcome::Fenced { .. }));
    assert_eq!(
        AsyncAuthorityStore::head(&authority, authority_id, &mut authority_work,)
            .await
            .expect("unchanged authority"),
        head
    );
}

#[tokio::test]
async fn bounded_publication_close_renewal_expiry_interleavings_preserve_peer_authority() {
    // Exhaustive serial linearizations of publication, close or renewal, and
    // expiry: twelve traces, two leases and two clock samples. This finite check
    // assumes atomic provider transactions and a trusted monotonic clock;
    // it makes no unbounded liveness or browser scheduling claim.
    for (events, renew) in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ]
    .into_iter()
    .flat_map(|events| [false, true].map(move |renew| (events, renew)))
    {
        let clock = Arc::new(TestClock::default());
        clock.0.store(10, Ordering::SeqCst);
        let stream = Arc::new(acyclic_stream::MemoryStream::new_with_clock(
            acyclic_stream::MemoryLimits::default(),
            clock.clone(),
        ));
        let authority = StreamAuthorityStore::new(Arc::clone(&stream));
        let windows = OperationWindowCoordinator::new(StreamOperationWindowStore::new(stream));
        let workspace = WorkspaceId::from_bytes([91; 16]);
        let authority_id = crate::kernel::volume_authority_id(workspace.volume_id());
        let cancellation = CancellationToken::new();
        let mut authority_work = crate::AuthorityWork::new(WorkBudget::UNBOUNDED, &cancellation);
        AsyncAuthorityStore::create_authority(
            &authority,
            authority_id,
            Epoch::GENESIS,
            &mut authority_work,
            None::<&mut ()>,
        )
        .await
        .expect("create");
        let first = windows
            .begin(workspace, generation(1), "first", 10, 20)
            .await
            .expect("first");
        let peer = windows
            .begin(workspace, generation(1), "peer", 10, 30)
            .await
            .expect("peer");
        let mut revoked = false;
        let mut renewed = None;
        for event in events {
            match event {
                0 => {
                    let head =
                        AsyncAuthorityStore::head(&authority, authority_id, &mut authority_work)
                            .await
                            .expect("head");
                    let allowed = !revoked && clock.0.load(Ordering::SeqCst) < 20;
                    let outcome = AsyncAuthorityStore::compare_and_append(
                        &authority,
                        crate::GuardedAppend {
                            authority_id,
                            epoch: head.epoch,
                            expected: head,
                            commit: ProposedCommit {
                                operation_id: OperationId::from_bytes([92; 16]),
                                fingerprint: Digest::from_bytes([93; 32]),
                                payload: Bytes::from_static(b"first"),
                            },
                            permit: first.publication_permit(),
                        },
                        &mut authority_work,
                        None::<&mut ()>,
                    )
                    .await
                    .expect("publish");
                    assert_eq!(
                        matches!(outcome, crate::AppendOutcome::Committed(_)),
                        allowed,
                        "trace {events:?}, renew={renew}"
                    );
                }
                1 => {
                    let now = clock.0.load(Ordering::SeqCst);
                    if renew {
                        let outcome = windows.renew(&first, now, 30).await;
                        if now < 20 {
                            renewed = Some(outcome.expect("renew live lease"));
                            revoked = true;
                        } else {
                            assert!(matches!(outcome, Err(OperationWindowError::StaleLease)));
                        }
                    } else {
                        windows.finish(&first, now).await.expect("close");
                        revoked = true;
                    }
                }
                _ => {
                    clock.0.store(20, Ordering::SeqCst);
                }
            }
        }
        for (index, lease) in std::iter::once(peer).chain(renewed).enumerate() {
            let head = AsyncAuthorityStore::head(&authority, authority_id, &mut authority_work)
                .await
                .expect("peer head");
            let outcome = AsyncAuthorityStore::compare_and_append(
                &authority,
                crate::GuardedAppend {
                    authority_id,
                    epoch: head.epoch,
                    expected: head,
                    commit: ProposedCommit {
                        operation_id: OperationId::from_bytes(
                            [94 + u8::try_from(index).expect("two leases"); 16],
                        ),
                        fingerprint: Digest::from_bytes(
                            [95 + u8::try_from(index).expect("two leases"); 32],
                        ),
                        payload: Bytes::from_static(b"peer"),
                    },
                    permit: lease.publication_permit(),
                },
                &mut authority_work,
                None::<&mut ()>,
            )
            .await
            .expect("peer publish");
            assert!(
                matches!(outcome, crate::AppendOutcome::Committed(_)),
                "trace {events:?}, renew={renew}"
            );
        }
    }
}

#[tokio::test]
async fn stream_renewal_fences_old_permit_before_its_original_deadline() {
    let clock = Arc::new(TestClock::default());
    clock.0.store(10, Ordering::SeqCst);
    let stream = Arc::new(acyclic_stream::MemoryStream::new_with_clock(
        acyclic_stream::MemoryLimits::default(),
        clock.clone(),
    ));
    let authority = StreamAuthorityStore::new(Arc::clone(&stream));
    let windows =
        OperationWindowCoordinator::new(StreamOperationWindowStore::new(Arc::clone(&stream)));
    let workspace = WorkspaceId::from_bytes([101; 16]);
    let authority_id = crate::kernel::volume_authority_id(workspace.volume_id());
    let cancellation = CancellationToken::new();
    let mut authority_work = crate::AuthorityWork::new(WorkBudget::UNBOUNDED, &cancellation);
    AsyncAuthorityStore::create_authority(
        &authority,
        authority_id,
        Epoch::GENESIS,
        &mut authority_work,
        None::<&mut ()>,
    )
    .await
    .expect("create authority");
    let original = windows
        .begin(workspace, generation(1), "original-owner", 10, 20)
        .await
        .expect("begin original lease");
    let peer = windows
        .begin(workspace, generation(9), "peer-owner", 10, 25)
        .await
        .expect("begin independent peer");
    assert_eq!(peer.pinned_parent, original.pinned_parent);
    // Begin's permit must work; the stale negative below cannot pass merely
    // because this provider rejects every managed publication.
    let mut current = original.clone();
    for index in 0_u8..6 {
        if index == 1 {
            clock.0.store(11, Ordering::SeqCst);
            let mut losing = windows
                .store()
                .load(workspace)
                .await
                .expect("snapshot")
                .expect("active");
            let expected_revision = losing.revision;
            losing.revision += 1;
            let OperationWindowPhase::Active { leases, .. } = &mut losing.phase else {
                panic!("active window");
            };
            leases
                .get_mut(&original.lease_id)
                .expect("lease")
                .expires_at_millis = 40;
            current = windows.renew(&original, 11, 30).await.expect("renew lease");
            assert!(
                !windows
                    .store()
                    .compare_and_swap(workspace, expected_revision, losing)
                    .await
                    .expect("losing CAS")
            );
            assert_eq!(current.lease_id, original.lease_id);
            assert_eq!(current.pinned_parent, original.pinned_parent);
            for expiry in [20, 30] {
                assert!(matches!(
                    windows.renew(&current, 11, expiry).await,
                    Err(OperationWindowError::InvalidExpiry)
                ));
            }
        }
        if index == 5 {
            assert!(matches!(
                windows
                    .finish(&current, 11)
                    .await
                    .expect("close renewed lease"),
                OperationWindowFinish::StillActive { remaining: 1 }
            ));
        }
        let permit = match index {
            2 | 5 => current.publication_permit(),
            3 => peer.publication_permit(),
            _ => original.publication_permit(),
        };
        let expected_commit = matches!(index, 0 | 2 | 3);
        let before = AsyncAuthorityStore::head(&authority, authority_id, &mut authority_work)
            .await
            .expect("head");
        let outcome = AsyncAuthorityStore::compare_and_append(
            &authority,
            crate::GuardedAppend {
                authority_id,
                epoch: before.epoch,
                expected: before,
                commit: ProposedCommit {
                    operation_id: OperationId::from_bytes(
                        [if index == 4 { 102 } else { 102 + index }; 16],
                    ),
                    fingerprint: Digest::from_bytes(
                        [if index == 4 { 104 } else { 104 + index }; 32],
                    ),
                    payload: Bytes::from_static(b"renewal-control"),
                },
                permit,
            },
            &mut authority_work,
            None::<&mut ()>,
        )
        .await
        .expect("guarded publication");
        if expected_commit {
            assert!(matches!(outcome, crate::AppendOutcome::Committed(_)));
        } else {
            if index == 4 {
                // The original commit remains recoverable after token rotation
                // and later peer publication, without appending another effect.
                assert!(matches!(outcome, crate::AppendOutcome::AlreadyCommitted(_)));
            } else {
                assert!(matches!(outcome, crate::AppendOutcome::Fenced { .. }));
            }
            assert_eq!(
                AsyncAuthorityStore::head(&authority, authority_id, &mut authority_work,)
                    .await
                    .expect("unchanged head"),
                before
            );
        }
    }
    let renewed = windows.renew(&original, 11, 30).await;
    assert!(matches!(renewed, Err(OperationWindowError::StaleLease)));
}

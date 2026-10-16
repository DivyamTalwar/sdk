//! Models the two linearization orders of publication and follow registration.
//! Authorization is pure and checked separately. Only the task-volume/provider
//! synchronization is constructed inside Loom; these use the real writer/query.
use super::fixtures::volume;
use super::*;
use crate::{
    AgentId,
    task_volume::{ChangeStream, Component, TaskFile},
};
use acyclic_stream::MemoryStream;
use futures::Stream;
use loom::sync::atomic::{AtomicBool, Ordering};
use std::{
    pin::Pin,
    task::{Context, Poll, Wake, Waker},
};

const _: fn(acyclic_stream::query_sync::SyncMutex<()>) -> loom::sync::Mutex<()> = |lock| lock;

struct Woken(AtomicBool);
impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn files() -> Arc<TaskFiles<MemoryStream>> {
    let volume = volume(AgentId::from_bytes([1; 16]));
    let inner = loom::future::block_on(Box::pin(TaskVolume::open(
        Arc::new(MemoryStream::default()),
        StreamPath::new("task-files-model").unwrap(),
    )))
    .unwrap();
    // The immutable authorization boundary has no shared state. The production
    // methods below still enforce the file role and exact byte/generation bounds.
    Arc::new(TaskFiles {
        inner,
        volume,
        maximum_bytes: 4096,
    })
}

// Independent live file views must arbitrate through the durable provider even
// when they cannot see one another's cache updates. Each index is still the
// production mutex; the shared provider's real locks linearize both writers.
fn reopen(files: &TaskFiles<MemoryStream>) -> Arc<TaskFiles<MemoryStream>> {
    let inner = loom::future::block_on(Box::pin(TaskVolume::open(
        files.inner.provider.clone(),
        files.inner.path.clone(),
    )))
    .unwrap();
    Arc::new(TaskFiles {
        inner,
        volume: files.volume.clone(),
        maximum_bytes: files.maximum_bytes,
    })
}

fn request() -> FileValue {
    FileValue::new(
        FileKey::Task {
            task: Component::new("task").unwrap(),
            file: TaskFile::Request(Component::new("request").unwrap()),
        },
        b"request".to_vec(),
    )
}
fn query() -> Query {
    Query::Requests(Component::new("task").unwrap())
}
fn next(
    stream: &mut ChangeStream,
    waker: &Waker,
) -> Poll<Option<super::super::Result<super::super::Change>>> {
    Pin::new(stream).poll_next(&mut Context::from_waker(waker))
}

fn gap(broken: bool) {
    let files = files();
    // These are the same two production methods called by snapshot_follow.
    // Force publication into their handoff interval instead of modeling all
    // unrelated backend operations before and after this boundary concurrently.
    let snapshot = loom::future::block_on(Box::pin(files.inner.snapshot(&query(), 8))).unwrap();
    let expected = request();
    let writer = files.clone();
    let write = expected.clone();
    loom::thread::spawn(move || {
        loom::future::block_on(Box::pin(writer.write(snapshot.generation, &write))).unwrap()
    })
    .join()
    .unwrap();
    let cursor = if broken {
        // Wrong: a second cut skips a write absent from the original snapshot.
        loom::future::block_on(Box::pin(files.inner.snapshot(&query(), 8)))
            .unwrap()
            .cursor
    } else {
        snapshot.cursor
    };
    let mut changes =
        loom::future::block_on(Box::pin(files.inner.follow(query(), cursor))).unwrap();
    let observed = match next(&mut changes, Waker::noop()) {
        Poll::Ready(Some(Ok(change))) => change.file,
        _ => None,
    };
    assert_eq!(
        observed.as_ref(),
        Some(&expected),
        "snapshot/follow skipped request"
    );
}

fn wake(broken: bool) {
    let files = files();
    let mut observation =
        loom::future::block_on(Box::pin(files.observe(query(), None, 8))).unwrap();
    let snapshot = observation.snapshot.take().unwrap();
    assert!(snapshot.files.is_empty());
    let expected = request();
    let writer = files.clone();
    let write = expected.clone();
    let producer = loom::thread::spawn(move || {
        loom::future::block_on(Box::pin(writer.write(snapshot.generation, &write))).unwrap()
    });
    let woken = Arc::new(Woken(AtomicBool::new(false)));
    let waker = Waker::from(woken.clone());
    let first = next(
        &mut observation.changes,
        if broken { Waker::noop() } else { &waker },
    );
    producer.join().unwrap();
    let observed = match first {
        Poll::Ready(Some(Ok(change))) => change.file,
        Poll::Pending => {
            assert!(
                woken.0.load(Ordering::SeqCst),
                "accepted request did not wake its follower"
            );
            match next(&mut observation.changes, &waker) {
                Poll::Ready(Some(Ok(change))) => change.file,
                _ => None,
            }
        }
        other => panic!("unexpected follow result: {other:?}"),
    };
    assert_eq!(
        observed.as_ref(),
        Some(&expected),
        "accepted request was not delivered"
    );
}

fn check(scenario: fn(bool), diagnostic: &str) {
    loom::model(move || scenario(false));
    let error = std::panic::catch_unwind(move || loom::model(move || scenario(true)))
        .expect_err("broken control was accepted");
    let message = error
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| error.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains(diagnostic),
        "unexpected negative control: {message}"
    );
}

#[test]
fn a_request_accepted_during_the_snapshot_handoff_is_replayed() {
    check(gap, "snapshot/follow skipped request");
}
#[test]
fn a_request_accepted_after_registration_wakes_and_reaches_its_follower() {
    check(wake, "accepted request did not wake its follower");
}

fn child_control_race(broken: bool) {
    use crate::task_volume::{ChildFile, Control};
    let files = files();
    let reference = Component::new("running-child").unwrap();
    let control_key = FileKey::Child {
        reference: reference.clone(),
        file: ChildFile::Control,
    };
    let snapshot = loom::future::block_on(Box::pin(files.observe(Query::Children, None, 8)))
        .unwrap()
        .snapshot
        .unwrap();
    let request = FileValue::new(
        FileKey::Child {
            reference,
            file: ChildFile::Request,
        },
        b"work".to_vec(),
    );
    let cut = loom::future::block_on(Box::pin(files.write(snapshot.generation, &request))).unwrap();
    let writer = reopen(&files);
    let key = control_key.clone();
    let cancel = loom::thread::spawn(move || {
        let expected = if broken {
            // Negative control: substitute a fresh cut for the caller's stale
            // observation. A pause and a conflicting cancel can then both win.
            loom::future::block_on(Box::pin(writer.observe(Query::Children, None, 8)))
                .unwrap()
                .snapshot
                .unwrap()
                .generation
        } else {
            cut
        };
        loom::future::block_on(Box::pin(writer.write(
            expected,
            &FileValue::new(key, Control::Cancel.bytes().to_vec()),
        )))
    });
    // The model thread is the second actor: a third thread that only joins
    // adds scheduler states without adding a publication interleaving.
    let paused = loom::future::block_on(Box::pin(files.write(
        cut,
        &FileValue::new(control_key.clone(), Control::Pause.bytes().to_vec()),
    )))
    .is_ok();
    let cancelled = cancel.join().unwrap().is_ok();
    assert_ne!(
        cancelled, paused,
        "conflicting child controls both committed"
    );
    let snapshot = loom::future::block_on(Box::pin(files.observe(Query::Children, None, 8)))
        .unwrap()
        .snapshot
        .unwrap();
    assert_eq!(snapshot.files.len(), 2);
    if cancelled {
        let resume = FileValue::new(control_key, Control::Resume.bytes().to_vec());
        assert!(matches!(
            loom::future::block_on(Box::pin(files.write(snapshot.generation, &resume))),
            Err(Error::ImmutableConflict(_))
        ));
    }
}

#[test]
fn concurrent_child_controls_use_the_running_child_cut() {
    check(
        child_control_race,
        "conflicting child controls both committed",
    );
}

fn child_file(file: crate::task_volume::ChildFile, bytes: Vec<u8>) -> FileValue {
    FileValue::new(
        FileKey::Child {
            reference: Component::new("worker").unwrap(),
            file,
        },
        bytes,
    )
}

fn child_status(status: crate::task_volume::ChildStatus) -> FileValue {
    child_file(
        crate::task_volume::ChildFile::Status,
        serde_json::to_vec(&status).unwrap(),
    )
}

fn running_child(
    settled: bool,
) -> (
    Arc<TaskFiles<MemoryStream>>,
    super::super::Generation,
    crate::task_volume::ChildStatus,
) {
    use crate::task_volume::{ChildFile, ChildState, ChildStatus};
    let files = files();
    let snapshot =
        loom::future::block_on(Box::pin(files.inner.snapshot(&Query::Children, 8))).unwrap();
    let cut = loom::future::block_on(Box::pin(files.write(
        snapshot.generation,
        &child_file(ChildFile::Request, b"work".to_vec()),
    )))
    .unwrap();
    let running = ChildStatus {
        task: crate::TaskId::from_bytes([7; 16]),
        state: ChildState::Running,
        settled,
        inputs: 1,
    };
    let cut =
        loom::future::block_on(Box::pin(files.inner.append(cut, &child_status(running)))).unwrap();
    (files, cut, running)
}

fn child_completion_race(broken: bool) {
    use crate::task_volume::{ChildFile, ChildStatus, Control};
    let (files, cut, running) = running_child(false);
    let cancelled = child_file(ChildFile::Control, Control::Cancel.bytes().to_vec());
    let writer = reopen(&files);
    let write = cancelled.clone();
    let controller = loom::thread::spawn(move || {
        loom::future::block_on(Box::pin(writer.write(cut, &write))).is_ok()
    });
    let completion = child_status(ChildStatus {
        settled: true,
        ..running
    });
    let (cancelled, completed) = if broken {
        // Negative control: lose a terminal control from a cache while using
        // its fresh cut. The actual validation, locks and provider still run.
        assert!(controller.join().unwrap());
        let snapshot =
            loom::future::block_on(Box::pin(files.inner.snapshot(&Query::Children, 8))).unwrap();
        loom::future::block_on(Box::pin(files.inner.index.lock()))
            .files
            .insert(
                cancelled.key.clone(),
                FileValue::new(cancelled.key.clone(), Control::Resume.bytes().to_vec()),
            );
        (
            true,
            loom::future::block_on(Box::pin(
                files.inner.append(snapshot.generation, &completion),
            ))
            .is_ok(),
        )
    } else {
        let completed =
            loom::future::block_on(Box::pin(files.inner.append(cut, &completion))).is_ok();
        (controller.join().unwrap(), completed)
    };
    assert_ne!(
        cancelled, completed,
        "conflicting child completion and control both committed"
    );
    let snapshot =
        loom::future::block_on(Box::pin(files.inner.snapshot(&Query::Children, 8))).unwrap();
    let observed = snapshot
        .files
        .iter()
        .find(|file| {
            matches!(
                file.key,
                FileKey::Child {
                    file: ChildFile::Status,
                    ..
                }
            )
        })
        .unwrap();
    assert_eq!(
        observed,
        &if completed {
            completion
        } else {
            child_status(running)
        }
    );
}

fn child_terminal_freeze(broken: bool) {
    use crate::task_volume::{ChildFile, ChildState, ChildStatus, Control};
    // These are exactly the two successful race outcomes above: the original
    // running status or its settled replacement. Exhaust the serial suffix for
    // each, outside the concurrent prefix's scheduling state space.
    for settled in [false, true] {
        let (files, cut, running) = running_child(settled);
        let cancelled = child_file(ChildFile::Control, Control::Cancel.bytes().to_vec());
        let cut = loom::future::block_on(Box::pin(files.write(cut, &cancelled))).unwrap();
        if broken {
            loom::future::block_on(Box::pin(files.inner.index.lock()))
                .files
                .insert(
                    cancelled.key.clone(),
                    FileValue::new(cancelled.key.clone(), Control::Resume.bytes().to_vec()),
                );
        }
        let completion = child_status(ChildStatus {
            settled: true,
            ..running
        });
        assert!(
            loom::future::block_on(Box::pin(files.inner.append(cut, &completion))).is_err(),
            "terminal control allowed a late running status"
        );
        let acknowledged = child_status(ChildStatus {
            state: ChildState::Cancelled,
            settled: true,
            ..running
        });
        let cut = loom::future::block_on(Box::pin(files.inner.append(cut, &acknowledged))).unwrap();
        let changed = child_status(ChildStatus {
            inputs: 2,
            state: ChildState::Cancelled,
            settled: true,
            ..running
        });
        assert!(loom::future::block_on(Box::pin(files.inner.append(cut, &changed))).is_err());
    }
}

#[test]
fn child_completion_is_fenced_by_control_until_a_frozen_quiescent_acknowledgement() {
    check(
        child_completion_race,
        "conflicting child completion and control both committed",
    );
    check(
        child_terminal_freeze,
        "terminal control allowed a late running status",
    );
}

fn parent_cancellation_race(broken: bool) {
    use crate::task_volume::ChildFile;
    use acyclic_stream::{AppendOutcome, AppendRequest, CommitCondition};
    let files = files();
    let provider = files.inner.provider.clone();
    let parent = StreamPath::new("parent-owner").unwrap();
    let seed = loom::future::block_on(Box::pin(provider.append(AppendRequest {
        path: parent.clone(),
        records: vec![b"running".to_vec().into()],
        if_tail: Some(0),
        idempotency_key: None,
    })))
    .unwrap();
    assert!(matches!(seed, AppendOutcome::Committed(_)));
    let condition = CommitCondition::Tail {
        path: parent.clone(),
        expected: 1,
    };
    let snapshot =
        loom::future::block_on(Box::pin(files.inner.snapshot(&Query::Children, 8))).unwrap();
    let cancel = loom::thread::spawn(move || {
        let result = loom::future::block_on(Box::pin(provider.append(AppendRequest {
            path: parent,
            records: vec![b"cancelled".to_vec().into()],
            if_tail: Some(1),
            idempotency_key: None,
        })))
        .unwrap();
        assert!(matches!(result, AppendOutcome::Committed(_)));
    });
    let request = |reference| {
        FileValue::new(
            FileKey::Child {
                reference: Component::new(reference).unwrap(),
                file: ChildFile::Request,
            },
            b"work".to_vec(),
        )
    };
    let first = request("first");
    let result = if broken {
        loom::future::block_on(Box::pin(files.write(snapshot.generation, &first)))
    } else {
        loom::future::block_on(Box::pin(files.write_if(
            snapshot.generation,
            &first,
            condition.clone(),
            None,
        )))
    };
    cancel.join().unwrap();
    let snapshot =
        loom::future::block_on(Box::pin(files.inner.snapshot(&Query::Children, 8))).unwrap();
    if result.is_ok() {
        assert_eq!(snapshot.files, vec![first]);
    } else {
        assert!(matches!(result, Err(Error::FenceConflict)));
        assert!(snapshot.files.is_empty());
    }
    let late = request("late");
    let result = if broken {
        // Negative control: omit the same owner condition from the real commit.
        loom::future::block_on(Box::pin(files.write(snapshot.generation, &late)))
    } else {
        loom::future::block_on(Box::pin(files.write_if(
            snapshot.generation,
            &late,
            condition,
            None,
        )))
    };
    assert!(
        matches!(result, Err(Error::FenceConflict)),
        "cancelled parent published a late child request"
    );
}

#[test]
fn parent_cancellation_fences_child_file_publication() {
    check(
        parent_cancellation_race,
        "cancelled parent published a late child request",
    );
}

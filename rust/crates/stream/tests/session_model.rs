//! Bounded verification of the canonical Streams session.
#![cfg(feature = "grpc")]

use acyclic_stream::{
    AppendRequest, MemoryStream, OwnedBytes, StreamError, StreamPath, StreamProvider, session,
    wire::{self, session_frame::Body, session_subscription::Kind},
};
use bytes::Bytes;
use futures::{StreamExt, channel::mpsc};
use loom::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

// Compile the production cursor's exact lock/transition methods against loom's
// Mutex. There is one lock rank, with no nested acquisition or awaiting guard.
#[path = "../src/session/client/progress.rs"]
mod progress;

#[test]
fn cursor_consumption_and_movement_are_atomic() {
    loom::model(|| {
        let cursor = Arc::new(progress::Progress::new(0));
        let old = Arc::clone(&cursor);
        let old = loom::thread::spawn(move || old.consume(1, 0));
        let moved = Arc::clone(&cursor);
        let moved = loom::thread::spawn(move || {
            let generation = moved.fence()?;
            let resume = moved.next()?;
            assert_eq!(generation, 2);
            let applied = moved.consume(generation, resume)?;
            assert!(applied);
            Ok::<_, StreamError>(resume)
        });
        let old_applied = old.join().unwrap().unwrap();
        let resume = moved.join().unwrap().unwrap();
        assert_eq!(resume, u64::from(old_applied));
        assert_eq!(cursor.next().unwrap(), resume + 1);
        assert!(!cursor.consume(1, resume + 1).unwrap());
        // Calling another lock-taking operation after every path also makes a
        // leaked guard or recursive lock acquisition a model-detected deadlock.
        assert_eq!(cursor.fence().unwrap(), 3);
    });
}

fn frame(id: u64, body: Body) -> wire::SessionFrame {
    wire::SessionFrame {
        channel_id: id,
        generation: u64::from(id != 0),
        body: Some(body),
    }
}
async fn append(provider: &MemoryStream) {
    provider
        .append(AppendRequest {
            path: StreamPath::new("events").unwrap(),
            records: vec![OwnedBytes::from_static(b"delta")],
            if_tail: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
}

#[test]
fn real_session_has_no_lost_append_or_credit_wakeup() {
    // Bound the state to one channel/record, not the explored schedules. The
    // production serve/follow/select path runs with loom's instrumented waker.
    for credit_first in [false, true] {
        loom::model(move || {
            let provider = std::sync::Arc::new(MemoryStream::default());
            loom::future::block_on(append(&provider));
            let (sender, receiver) = mpsc::unbounded();
            sender
                .unbounded_send(Ok(frame(
                    0,
                    Body::Hello(wire::SessionHello {
                        version: 1,
                        client_id: Bytes::from_static(b"model"),
                        destination_node: "node".into(),
                        ..Default::default()
                    }),
                )))
                .unwrap();
            sender
                .unbounded_send(Ok(frame(
                    0,
                    Body::Interest(wire::SessionInterest {
                        revision: 1,
                        upsert: vec![wire::SessionSubscription {
                            channel: Some(wire::SessionChannel {
                                channel_id: 1,
                                generation: 1,
                            }),
                            resume: Some(wire::SessionCursor {
                                next: 1,
                                ..Default::default()
                            }),
                            kind: Some(Kind::Follow(wire::SessionFollow {
                                path: "events".into(),
                                delivery: wire::SessionDelivery::All as i32,
                            })),
                        }],
                        remove: Vec::new(),
                    }),
                )))
                .unwrap();
            let mut output = session::serve(
                std::sync::Arc::clone(&provider),
                "node".into(),
                session::Limits::default(),
                receiver.boxed(),
            )
            .unwrap();
            let consumer = loom::thread::spawn(move || {
                loom::future::block_on(async {
                    loop {
                        let received = output.next().await.unwrap().unwrap();
                        if let Some(Body::Data(data)) = received.body {
                            assert_eq!(data.after.unwrap().next, 2);
                            break;
                        }
                    }
                });
            });
            let grant = || {
                for id in [0, 1] {
                    sender
                        .unbounded_send(Ok(frame(
                            id,
                            Body::Window(wire::SessionWindow {
                                bytes: 1_000_000,
                                frames: 1,
                            }),
                        )))
                        .unwrap();
                    loom::thread::yield_now();
                }
            };
            if credit_first {
                grant();
            }
            loom::future::block_on(append(&provider));
            if !credit_first {
                grant();
            }
            consumer.join().unwrap();
            drop(sender);
        });
    }
}

#[test]
fn channel_allocation_is_unique_and_never_wraps() {
    for start in [1, u64::MAX - 2] {
        loom::model(move || {
            let counter = Arc::new(AtomicU64::new(start));
            let first = Arc::clone(&counter);
            let first = loom::thread::spawn(move || progress::allocate(&first));
            let second = Arc::clone(&counter);
            let second = loom::thread::spawn(move || progress::allocate(&second));
            let ids: Vec<_> = [first.join().unwrap(), second.join().unwrap()]
                .into_iter()
                .flatten()
                .collect();
            assert!(ids.iter().all(|id| *id != 0 && id % 2 == 1));
            if ids.len() == 2 {
                assert_ne!(ids[0], ids[1]);
            }
            assert_eq!(
                counter.load(Ordering::Relaxed),
                start + 2 * ids.len() as u64
            );
            assert_eq!(ids.len(), if start == 1 { 2 } else { 1 });
        });
    }
}

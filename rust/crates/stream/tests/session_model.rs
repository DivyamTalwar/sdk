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
use prost::Message as _;

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

type ReleaseSender = loom::sync::mpsc::Sender<credit::Released>;
#[path = "../src/session/client/credit.rs"]
mod credit;

#[test]
fn discarded_and_consumed_deliveries_return_credit_exactly_once() {
    loom::model(|| {
        let (sender, receiver) = loom::sync::mpsc::channel();
        // One lock rank, receiver storage. It is released before disposing any
        // delivery; credit Drop only sends and never takes the storage lock.
        let storage = Arc::new(Mutex::new(
            (0..2)
                .map(|id| {
                    credit::Credit::new(
                        sender.clone(),
                        credit::Released {
                            id,
                            generation: 1,
                            node: "node".into(),
                            epoch: 1,
                            bytes: 100 + id,
                        },
                    )
                })
                .collect::<Vec<_>>(),
        ));
        let consuming = Arc::clone(&storage);
        let consuming = loom::thread::spawn(move || {
            let delivery = consuming.lock().unwrap().pop();
            loom::thread::yield_now();
            drop(delivery);
        });
        let dropping = Arc::clone(&storage);
        let dropping = loom::thread::spawn(move || {
            let discarded = std::mem::take(&mut *dropping.lock().unwrap());
            drop(discarded);
        });
        consuming.join().unwrap();
        dropping.join().unwrap();
        let mut bytes = 0;
        let mut ids = Vec::new();
        for _ in 0..2 {
            let released = receiver.recv().unwrap();
            assert_eq!(
                (released.generation, released.epoch, released.node.as_str()),
                (1, 1, "node")
            );
            bytes += released.bytes;
            ids.push(released.id);
        }
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1]);
        assert_eq!(bytes, 201);
        assert!(storage.lock().unwrap().is_empty());
    });
}

#[test]
fn a_saturated_channel_cannot_block_an_independent_follow() {
    for latest in [false, true] {
        loom::model(move || {
            let provider = std::sync::Arc::new(MemoryStream::default());
            let status = || {
                Bytes::from(
                    wire::SessionLatestValue {
                        key: Bytes::from_static(b"agent"),
                        value: Bytes::from_static(b"delta"),
                        attention: true,
                    }
                    .encode_to_vec(),
                )
            };
            let append_at = |path: &'static str| {
                let provider = std::sync::Arc::clone(&provider);
                let value = status();
                async move {
                    provider
                        .append(AppendRequest {
                            path: StreamPath::new(path).unwrap(),
                            records: vec![OwnedBytes::from(value)],
                            if_tail: None,
                            idempotency_key: None,
                        })
                        .await
                        .unwrap();
                }
            };
            loom::future::block_on(append_at("slow"));
            loom::future::block_on(append_at("healthy"));
            let (sender, receiver) = mpsc::unbounded();
            sender
                .unbounded_send(Ok(frame(
                    0,
                    Body::Hello(wire::SessionHello {
                        version: 1,
                        client_id: Bytes::from_static(b"model"),
                        destination_node: "node".into(),
                        receive_bytes: 1_000_000,
                        receive_frames: 100,
                    }),
                )))
                .unwrap();
            let subscriptions = [(1, "slow", latest), (3, "healthy", false)]
                .into_iter()
                .map(|(id, path, latest)| wire::SessionSubscription {
                    channel: Some(wire::SessionChannel {
                        channel_id: id,
                        generation: 1,
                    }),
                    resume: Some(wire::SessionCursor {
                        next: 1,
                        ..Default::default()
                    }),
                    kind: Some(Kind::Follow(wire::SessionFollow {
                        path: path.into(),
                        delivery: if latest {
                            wire::SessionDelivery::Latest
                        } else {
                            wire::SessionDelivery::All
                        } as i32,
                    })),
                })
                .collect();
            sender
                .unbounded_send(Ok(frame(
                    0,
                    Body::Interest(wire::SessionInterest {
                        revision: 1,
                        upsert: subscriptions,
                        remove: Vec::new(),
                    }),
                )))
                .unwrap();
            let mut output = session::serve(
                std::sync::Arc::clone(&provider),
                "node".into(),
                session::Limits {
                    pending_frames: 2,
                    ..session::Limits::default()
                },
                receiver.boxed(),
            )
            .unwrap();
            // Prime both opens, then fill the stalled source before the
            // concurrent credit/append race. This bounds records, not schedules.
            loom::future::block_on(async {
                for _ in 0..4 {
                    output.next().await.unwrap().unwrap();
                }
            });
            for _ in 0..3 {
                loom::future::block_on(append_at("slow"));
            }
            loom::future::block_on(async {
                assert!(futures::poll!(output.next()).is_pending());
            });
            let consumer = loom::thread::spawn(move || {
                loom::future::block_on(async {
                    loop {
                        let received = output.next().await.unwrap().unwrap();
                        if let Some(Body::Data(data)) = received.body {
                            assert_eq!(received.channel_id, 3);
                            assert_eq!(data.after.unwrap().next, 2);
                            break;
                        }
                    }
                })
            });
            sender
                .unbounded_send(Ok(frame(
                    3,
                    Body::Window(wire::SessionWindow {
                        bytes: 1_000_000,
                        frames: 1,
                    }),
                )))
                .unwrap();
            loom::future::block_on(append_at("healthy"));
            consumer.join().unwrap();
            drop(sender);
        });
    }
}

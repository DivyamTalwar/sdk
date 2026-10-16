//! Properties and interleavings of the actual metadata client handshake.

#![cfg(not(target_arch = "wasm32"))]

#[path = "../src/native_mount/projfs/tests/metadata_output.rs"]
mod output;

use proptest::prelude::*;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use tokio::io::{AsyncRead, AsyncReadExt, BufReader, ReadBuf};

struct Chunks<'a>(&'a [u8], usize);

impl AsyncRead for Chunks<'_> {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let count = this.0.len().min(this.1).min(output.remaining());
        output.put_slice(&this.0[..count]);
        this.0 = &this.0[count..];
        Poll::Ready(Ok(()))
    }
}

fn completed<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("an in-memory reader cannot block"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn readiness_round_trips_without_consuming_the_completion(
        noise in proptest::collection::vec("[a-zé漢🙂]{0,24}", 0..8),
        newline in prop_oneof![Just("\n"), Just("\r\n")],
        suffix in proptest::collection::vec(any::<u8>(), 0..64),
        chunk in 1_usize..64,
    ) {
        let mut bytes = noise.iter().map(|line| format!("{line}{newline}")).collect::<String>().into_bytes();
        bytes.extend_from_slice(output::READY.as_bytes());
        bytes.extend_from_slice(newline.as_bytes());
        bytes.extend_from_slice(&suffix);
        let mut reader = BufReader::new(Chunks(&bytes, chunk));
        let mut line = String::new();
        let mut observed = Vec::new();
        prop_assert!(completed(output::read_output(&mut reader, &mut line, true, |line| observed.push(line.to_owned()))).unwrap());
        prop_assert!(line.is_empty());
        let mut remaining = Vec::new();
        completed(reader.read_to_end(&mut remaining)).unwrap();
        prop_assert_eq!(remaining, suffix);
        prop_assert_eq!(observed, noise.iter().map(|line| format!("{line}{newline}")).collect::<Vec<_>>());
    }
}

#[test]
fn failed_readiness_never_runs_the_boundary_or_sends_go() {
    let result: io::Result<()> = completed(output::after_ready(
        std::future::ready(Err(io::ErrorKind::UnexpectedEof.into())),
        || panic!("boundary ran before readiness"),
        || panic!("go sent before readiness"),
    ));
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    let result: io::Result<()> = completed(output::after_ready(
        std::future::ready(Ok(())),
        || Err(io::ErrorKind::PermissionDenied.into()),
        || panic!("go sent after a failed boundary"),
    ));
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
}

mod interleavings {
    use super::*;
    use loom::sync::{Arc, Condvar, Mutex};
    use std::collections::VecDeque;

    #[derive(Default)]
    struct State {
        bytes: VecDeque<u8>,
        eof: bool,
        reader: Option<Waker>,
        prepared: bool,
        boundary: bool,
        finished: bool,
        go: bool,
    }

    impl State {
        fn publish(&mut self, bytes: &[u8], eof: bool) -> Option<Waker> {
            self.prepared = true;
            self.bytes.extend(bytes);
            self.eof = eof;
            self.reader.take()
        }
    }

    #[derive(Default)]
    struct Pipe {
        // Only one modeled mutex; no nesting. The I/O adapter publishes bytes
        // and registers readers under it, and wakes only after releasing it.
        state: Mutex<State>,
        go: Condvar,
    }

    impl Pipe {
        fn publish(&self, bytes: &[u8], eof: bool) {
            let wake = self.state.lock().unwrap().publish(bytes, eof);
            if let Some(wake) = wake {
                wake.wake();
            }
        }
    }

    struct Reader(Arc<Pipe>);

    impl AsyncRead for Reader {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let mut state = self.0.state.lock().unwrap();
            if state.bytes.is_empty() && !state.eof {
                state.reader = Some(cx.waker().clone());
                return Poll::Pending;
            }
            while output.remaining() != 0 {
                let Some(byte) = state.bytes.pop_front() else {
                    break;
                };
                output.put_slice(&[byte]);
            }
            Poll::Ready(Ok(()))
        }
    }

    fn check(split: usize, bypass_ready: bool, missing_ready: bool) {
        loom::model(move || {
            let pipe = Arc::new(Pipe::default());
            let child = {
                let pipe = pipe.clone();
                loom::thread::spawn(move || {
                    if missing_ready {
                        pipe.publish(b"client failed\n", true);
                        return;
                    }
                    let marker = format!("{}\n", output::READY);
                    pipe.publish(&marker.as_bytes()[..split], false);
                    pipe.publish(&marker.as_bytes()[split..], false);
                    let wake = {
                        let mut state = pipe.state.lock().unwrap();
                        while !state.go {
                            state = pipe.go.wait(state).unwrap();
                        }
                        assert!(state.boundary, "go preceded the boundary");
                        state.finished = true;
                        state.publish(b"completed\n", true)
                    };
                    if let Some(wake) = wake {
                        wake.wake();
                    }
                })
            };
            let mut reader = BufReader::new(Reader(pipe.clone()));
            let mut line = String::new();
            let ordered = std::cell::Cell::new(false);
            // The real protocol contains no Rust locks. Its external I/O is
            // supplied by the instrumented adapter above, never Tokio locks.
            loom::future::block_on(async {
                let ready = async {
                    if bypass_ready
                        || output::read_output(&mut reader, &mut line, true, |_| {}).await?
                    {
                        Ok(())
                    } else {
                        Err(io::ErrorKind::UnexpectedEof.into())
                    }
                };
                let result: io::Result<()> = output::after_ready(
                    ready,
                    || {
                        let mut state = pipe.state.lock().unwrap();
                        ordered.set(state.prepared);
                        state.boundary = true;
                        Ok(())
                    },
                    || {
                        {
                            let mut state = pipe.state.lock().unwrap();
                            assert!(state.boundary);
                            state.go = true;
                        }
                        pipe.go.notify_one();
                        Ok(())
                    },
                )
                .await;
                if missing_ready {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
                    let state = pipe.state.lock().unwrap();
                    assert!(state.prepared);
                    assert!(!state.boundary);
                    assert!(!state.go);
                } else {
                    result.unwrap();
                    assert!(
                        !output::read_output(&mut reader, &mut line, false, |_| {})
                            .await
                            .unwrap()
                    );
                    assert!(pipe.state.lock().unwrap().finished);
                }
            });
            child.join().unwrap();
            // Assert after joining and releasing modeled objects: unwinding
            // queued Loom children can otherwise abort the Windows runner.
            drop(reader);
            drop(pipe);
            assert_eq!(
                ordered.get(),
                !missing_ready,
                "boundary ran before readiness"
            );
        });
    }

    #[test]
    fn fragmented_readiness_orders_the_boundary_and_completion_without_lost_wakes() {
        // Two fragments bound the model size without restricting schedules.
        // The property above varies byte boundaries and UTF-8 fragmentation.
        check(1, false, false);
    }

    #[test]
    fn eof_before_readiness_cannot_release_the_child() {
        check(0, false, true);
    }

    #[test]
    #[should_panic(expected = "boundary ran before readiness")]
    fn bypassing_readiness_is_detected() {
        check(1, true, false);
    }
}

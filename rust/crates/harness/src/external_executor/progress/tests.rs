//! The provider's atomic cursor and push are the contract at this boundary;
//! Shuttle instruments every state lock and wake in this small provider.
use super::*;
use shuttle::{
    future,
    sync::{Arc, Mutex},
};
use std::task::{Context, Poll, Waker};

#[derive(Default)]
struct State {
    ready: bool,
    revision: u64,
    wake: Option<Waker>,
}
struct Changes {
    state: Arc<Mutex<State>>,
    cursor: u64,
}

impl Stream for Changes {
    type Item = Result<()>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let revision = {
            let mut state = self.state.lock().unwrap();
            if self.cursor == state.revision {
                state.wake = Some(cx.waker().clone());
                return Poll::Pending;
            }
            state.revision
        };
        self.cursor = revision;
        Poll::Ready(Some(Ok(())))
    }
}

fn model(broken: bool) {
    shuttle::check_dfs(
        move || {
            future::block_on(async {
                // Lock order: sole provider-state lock. Drop before yielding/waking.
                let state = Arc::new(Mutex::new(State::default()));
                let writer = state.clone();
                let producer = future::spawn(async move {
                    future::yield_now().await;
                    let wake = {
                        let mut state = writer.lock().unwrap();
                        state.ready = true;
                        state.revision += 1;
                        state.wake.take()
                    };
                    if let Some(wake) = wake {
                        wake.wake();
                    }
                });
                let step = || async {
                    let ready = state.lock().unwrap().ready;
                    future::yield_now().await;
                    Ok(ready.then_some(7))
                };
                let subscribe = async {
                    let cursor = state.lock().unwrap().revision;
                    future::yield_now().await;
                    Ok(Changes {
                        state: state.clone(),
                        cursor,
                    })
                };
                let result = if broken {
                    // Mutation of the real algorithm: omit only its second check.
                    match step().await.unwrap() {
                        Some(value) => Some(value),
                        None => {
                            let mut changes = subscribe.await.unwrap();
                            changes.next().await.unwrap().unwrap();
                            step().await.unwrap()
                        }
                    }
                } else {
                    follow(step, subscribe, std::future::pending())
                        .await
                        .unwrap()
                };
                assert_eq!(result, Some(7));
                producer.await.unwrap();
            });
        },
        None,
    );
}

#[test]
fn passive_completion_between_snapshot_and_follow_cannot_be_lost() {
    model(false);
}

#[test]
#[should_panic(expected = "deadlock")]
fn negative_control_detects_omitting_the_second_read() {
    model(true);
}

#[test]
fn shutdown_does_not_drop_an_active_step() {
    shuttle::check_dfs(
        || {
            future::block_on(async {
                let finished = Arc::new(Mutex::new(false));
                let result = follow(
                    || async {
                        future::yield_now().await;
                        *finished.lock().unwrap() = true;
                        Ok::<_, crate::Error>(None::<()>)
                    },
                    async { Ok(futures::stream::pending::<Result<()>>()) },
                    async { Ok(()) },
                )
                .await
                .unwrap();
                assert!(finished.lock().unwrap().to_owned());
                assert_eq!(result, None);
            })
        },
        None,
    );
}

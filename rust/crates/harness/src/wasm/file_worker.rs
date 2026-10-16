//! Single event-loop owner; no shared mutable client or lock crosses provider I/O.
use super::{
    file_client::{admission_error, error},
    query::WasmTaskVolume,
};
use crate::client::mailbox;
use crate::{
    client::{FileClient, FileClientError},
    task_volume::{ChangeStream, Cursor, FileKey, Query},
};
use acyclic_stream::StreamProvider;
use futures::{FutureExt as _, StreamExt as _};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::prelude::*;

type Inner = FileClient<dyn StreamProvider, dyn StreamProvider>;
enum Wake {
    Command(Option<Command>),
    Connected(Result<u64, acyclic_stream::StreamError>),
    Published(Option<Result<crate::task_volume::Change, crate::task_volume::Error>>),
}
pub(super) enum Command {
    Enqueue {
        key: FileKey,
        bytes: js_sys::Uint8Array,
        reply: mailbox::Sender<Result<(), JsValue>>,
    },
    Flush(mailbox::Sender<Result<usize, JsValue>>),
    Idle(mailbox::Sender<Result<(), JsValue>>),
}
pub(super) struct State {
    pub(super) pending: Cell<usize>,
    pub(super) cursor: RefCell<Cursor>,
    pub(super) error: RefCell<JsValue>,
}
impl State {
    fn refresh(&self, inner: &Inner) {
        self.pending.set(inner.pending());
        *self.cursor.borrow_mut() = inner.cursor().clone();
    }
}
pub(super) async fn start(
    mut inner: Inner,
    target: &WasmTaskVolume,
) -> Result<(mailbox::Sender<Command>, Rc<State>), JsValue> {
    let connections = target.connections.clone();
    // A target file follow owns the session while offline intent is retained.
    // It consumes credit without needing a renderer to be attached.
    let mut changes: ChangeStream = if connections.is_some() {
        let cursor = target.volume.cut(&Query::Tasks).await.map_err(error)?;
        target
            .volume
            .follow(Query::Tasks, cursor)
            .await
            .map_err(error)?
    } else {
        Box::pin(futures::stream::pending())
    };
    let state = Rc::new(State {
        pending: Cell::new(inner.pending()),
        cursor: RefCell::new(inner.cursor().clone()),
        error: RefCell::new(JsValue::UNDEFINED),
    });
    let (commands, mut receiver) = mailbox::channel();
    let current = state.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let mut generation = 0;
        let mut idle: Option<mailbox::Sender<Result<(), JsValue>>> = None;
        loop {
            let event = {
                let observed = generation;
                let recovered = async {
                    match &connections {
                        Some(connections) => connections.changed(observed).await,
                        None => futures::future::pending().await,
                    }
                }
                .fuse();
                futures::pin_mut!(recovered);
                futures::select! {
                    command = receiver.recv().fuse() => Wake::Command(command),
                    connection = recovered => Wake::Connected(connection),
                    change = changes.next().fuse() => Wake::Published(change),
                }
            };
            let automatic = match event {
                Wake::Command(command) => {
                    let Some(command) = command else {
                        return;
                    };
                    match command {
                        Command::Enqueue { key, bytes, reply } => {
                            let result = inner
                                .enqueue_with(key, u64::from(bytes.length()), || {
                                    let bytes = bytes.to_vec();
                                    crate::client::validate_offline_request(&bytes)?;
                                    Ok(bytes.into())
                                })
                                .await
                                .map_err(admission_error);
                            current.refresh(&inner);
                            let admitted = result.is_ok();
                            let _ = reply.reply(result);
                            admitted && connections.is_some()
                        }
                        Command::Flush(reply) => {
                            let result = inner.flush().await.map_err(admission_error);
                            current.refresh(&inner);
                            let _ = reply.reply(result);
                            false
                        }
                        Command::Idle(reply) => {
                            if inner.pending() == 0 {
                                let _ = reply.reply(Ok(()));
                            } else if idle.as_ref().is_some_and(|reply| !reply.is_closed()) {
                                let _ =
                                    reply.reply(Err(error("an idle wait is already registered")));
                            } else {
                                idle = Some(reply);
                            }
                            false
                        }
                    }
                }
                Wake::Connected(connection) => match connection {
                    Ok(next) => {
                        generation = next;
                        true
                    }
                    Err(failure) => {
                        *current.error.borrow_mut() = error(failure);
                        false
                    }
                },
                Wake::Published(change) => {
                    let ended = change.is_none();
                    if let Some(Err(failure)) = change {
                        *current.error.borrow_mut() = error(failure);
                    }
                    if ended {
                        changes = Box::pin(futures::stream::pending());
                    }
                    !ended && inner.pending() > 0
                }
            };
            if automatic {
                // Rejections retire in Rust, then later requests proceed.
                // Indeterminate publication waits for a pushed recovery.
                loop {
                    match inner.flush().await {
                        Ok(_) => break,
                        Err(failure @ FileClientError::Rejected { .. }) => {
                            *current.error.borrow_mut() = admission_error(failure);
                        }
                        Err(failure) => {
                            *current.error.borrow_mut() = admission_error(failure);
                            break;
                        }
                    }
                }
                current.refresh(&inner);
            }
            if inner.pending() == 0
                && let Some(idle) = idle.take()
            {
                let _ = idle.reply(Ok(()));
            }
        }
    });
    Ok((commands, state))
}

use std::io::{self, Write};

#[cfg(feature = "model-checking")]
pub(crate) use loom::sync::{Arc, Mutex};
#[cfg(not(feature = "model-checking"))]
pub(crate) use std::sync::{Arc, Mutex};

use super::{Capabilities, RestoreGuard};

pub(crate) struct State<W: Write> {
    pub(crate) guard: RestoreGuard<W>,
    active: bool,
    closed: bool,
}

impl<W: Write> State<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            guard: RestoreGuard::new(writer, &Capabilities::default()),
            active: true,
            closed: false,
        }
    }
}

// Lock order: this is the sole terminal-state lock. Never acquire a signal
// iterator lock, wait for input, or join the signal worker while holding it.
// Output, mode mutations and lifecycle callbacks ALL enter through these paths.
pub(crate) fn active<W: Write, T>(
    state: &Mutex<State<W>>,
    action: impl FnOnce(&mut RestoreGuard<W>) -> io::Result<T>,
) -> io::Result<T> {
    let mut state = state.lock().map_err(|_| io::ErrorKind::Other)?;
    if state.closed {
        return Err(io::ErrorKind::NotConnected.into());
    }
    if !state.active {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    action(&mut state.guard)
}

pub(crate) fn restore<W: Write>(
    state: &Mutex<State<W>>,
    terminate: bool,
    os_restore: impl FnOnce(&mut W) -> io::Result<()>,
) -> io::Result<()> {
    let mut state = state.lock().map_err(|_| io::ErrorKind::Other)?;
    state.closed |= terminate;
    state.active = false;
    let dec = state.guard.restore();
    let os = os_restore(state.guard.writer());
    dec.and(os)
}

#[cfg(any(unix, feature = "model-checking"))]
pub(crate) fn resume<W: Write>(
    state: &Mutex<State<W>>,
    os_reapply: impl FnOnce(&mut W) -> io::Result<()>,
) -> io::Result<()> {
    let mut state = state.lock().map_err(|_| io::ErrorKind::Other)?;
    if !state.closed && !state.active {
        os_reapply(state.guard.writer())?;
        state.guard.reapply()?;
        state.active = true;
    }
    Ok(())
}

#[cfg(all(test, feature = "model-checking"))]
mod terminal_lifecycle_model {
    use super::*;
    use crate::term::ModeState;

    fn model(broken: bool) {
        loom::model(move || {
            let mut state = State::new(Vec::new());
            let caps = Capabilities {
                modes: [(2004, ModeState::Reset)].into(),
                ..Capabilities::default()
            };
            assert!(state.guard.detected(&caps).is_ok());
            let shared = Arc::new(Mutex::new(state));
            let input = Arc::clone(&shared);
            let worker = loom::thread::spawn(move || {
                if broken {
                    // Negative control: checking the close fence outside the
                    // mutation's critical section permits a post-close enable.
                    let permitted = input.lock().is_ok_and(|state| !state.closed);
                    loom::thread::yield_now();
                    if permitted && let Ok(mut state) = input.lock() {
                        assert!(state.guard.enable_mode(2004).is_ok());
                    }
                } else {
                    let _ = active(&input, |guard| guard.enable_mode(2004));
                }
            });
            assert!(restore(&shared, true, |_| Ok(())).is_ok());
            assert!(worker.join().is_ok());
            let mut state = match shared.lock() {
                Ok(state) => state,
                Err(_) => unreachable!(),
            };
            let bytes = state.guard.writer();
            let last_set = bytes.windows(8).rposition(|b| b == b"\x1b[?2004h");
            let last_reset = bytes.windows(8).rposition(|b| b == b"\x1b[?2004l");
            assert!(
                last_set.is_none() || last_reset > last_set,
                "mode enabled after terminal close"
            );
            assert!(state.closed);
        });
    }

    #[test]
    fn close_fences_every_mode_write() {
        model(false);
    }

    #[test]
    fn negative_control_detects_split_close_fence() {
        assert!(std::panic::catch_unwind(|| model(true)).is_err());
    }

    #[test]
    fn suspend_resume_and_close_cannot_reenable_closed_terminal() {
        loom::model(|| {
            let state = Arc::new(Mutex::new(State::new(Vec::new())));
            let resumed = Arc::clone(&state);
            let worker = loom::thread::spawn(move || {
                assert!(restore(&resumed, false, |_| Ok(())).is_ok());
                assert!(resume(&resumed, |_| Ok(())).is_ok());
            });
            assert!(restore(&state, true, |_| Ok(())).is_ok());
            assert!(worker.join().is_ok());
            assert!(active(&state, |_| Ok(())).is_err());
        });
    }
}

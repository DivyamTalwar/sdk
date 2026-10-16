use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::sync::OnceLock;
use std::time::Instant;

use super::lifecycle::{self, Arc, Mutex, State};
use super::{Capabilities, Decoder, Environment, PROBE_TIMEOUT, ProbeIo, ScreenMode, detect};
use crate::key_event::InputEvent;

use acyclic_terminal_console as ffi;

struct ConsoleIo {
    input: File,
    output: File,
    original: ffi::ConsoleState,
    pending: Option<ffi::PendingWrite>,
}
impl ConsoleIo {
    fn apply(&mut self, state: ffi::ConsoleState) -> io::Result<()> {
        ffi::console_apply(
            self.input.as_raw_handle() as usize,
            self.output.as_raw_handle() as usize,
            state,
        )
        .map_err(io::Error::other)
    }
    fn restore(&mut self) -> io::Result<()> {
        self.cancel_pending();
        let cancel =
            ffi::console_cancel(self.output.as_raw_handle() as usize).map_err(io::Error::other);
        let restore = self.apply(self.original);
        cancel.and(restore)
    }
    fn cancel_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            ffi::console_write_cancel(&pending);
        }
    }
    fn reapply(&mut self) -> io::Result<()> {
        let mut raw = self.original;
        // Disable cooked processing/quick edit; request VT input and output.
        raw.input_mode = (raw.input_mode & !(1 | 2 | 4 | 64)) | 8 | 128 | 512;
        raw.output_mode |= 1 | 4;
        raw.input_page = 65001;
        raw.output_page = 65001;
        self.apply(raw)
    }
}
impl Write for ConsoleIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Journal restoration writes cancel the pending application write
        // before emitting any undo bytes on the same output handle.
        self.cancel_pending();
        ffi::console_cancel(self.output.as_raw_handle() as usize).map_err(io::Error::other)?;
        let pending = ffi::console_write_start(self.output.as_raw_handle() as usize, bytes)
            .map_err(io::Error::other)?;
        finish_write(&pending, Instant::now() + PROBE_TIMEOUT)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn finish_write(pending: &ffi::PendingWrite, deadline: Instant) -> io::Result<usize> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout = u32::try_from(remaining.as_millis()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let result = ffi::console_write_wait(pending, timeout);
    if Instant::now() >= deadline || result.error == 1460 {
        return Err(io::ErrorKind::TimedOut.into());
    }
    if result.error != 0 {
        return Err(io::Error::from_raw_os_error(result.error as i32));
    }
    Ok(result.written as usize)
}

// Publication occurs before registering the OS handler and before any mode
// mutation. Afterwards this immutable owner is used only to reach the modeled
// state lock. Never register or unregister a handler while holding that lock.
static CONSOLE: OnceLock<Arc<Mutex<State<ConsoleIo>>>> = OnceLock::new();

fn console_control(event: u32) -> bool {
    if matches!(event, 0 | 1 | 2 | 5 | 6)
        && let Some(state) = CONSOLE.get()
    {
        if lifecycle::restore(state, true, ConsoleIo::restore).is_err() {
            let _ = lifecycle::restore(state, true, ConsoleIo::restore);
        }
    }
    #[cfg(test)]
    tests::observe_restored_console(event);
    // Continue to the default OS handler only AFTER restoration completes.
    false
}

/// Windows VT/UTF-8 console lifecycle with synchronous close/break restoration.
/// One process owns one console session, including when stdout is redirected.
pub struct Tty {
    state: Arc<Mutex<State<ConsoleIo>>>,
    decoder: Decoder,
    capabilities: Option<Capabilities>,
}
impl Tty {
    /// Capture original modes and code pages before enabling VT/UTF-8.
    pub fn open() -> io::Result<Self> {
        let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
        let output = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x40000000) // FILE_FLAG_OVERLAPPED
            .open("CONOUT$")?;
        let original = ffi::console_state(
            input.as_raw_handle() as usize,
            output.as_raw_handle() as usize,
        )
        .map_err(io::Error::other)?;
        let state = Arc::new(Mutex::new(State::new(ConsoleIo {
            input,
            output,
            original,
            pending: None,
        })));
        CONSOLE
            .set(Arc::clone(&state))
            .map_err(|_| io::ErrorKind::AlreadyExists)?;
        ffi::console_register(console_control)?;
        if let Err(error) = lifecycle::active(&state, |guard| guard.writer().reapply()) {
            let _ = lifecycle::restore(&state, true, ConsoleIo::restore);
            return Err(error);
        }
        Ok(Self {
            state,
            decoder: Decoder::default(),
            capabilities: None,
        })
    }
    /// Probe once using the whole-stream decoder retained for input.
    pub fn negotiate(&mut self, env: &Environment) -> io::Result<&Capabilities> {
        if self.capabilities.is_none() {
            let (caps, decoder) = detect(self, env)?;
            self.capabilities = Some(caps);
            self.decoder = decoder;
        }
        self.capabilities
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected.into())
    }
    /// Enable only modes whose original state is known.
    pub fn setup(&mut self, screen: ScreenMode) -> io::Result<()> {
        lifecycle::active(&self.state, |guard| guard.setup(screen))
    }
    /// Enable an additional queried mode under the same restoration journal.
    pub fn enable_mode(&mut self, mode: u16) -> io::Result<()> {
        lifecycle::active(&self.state, |guard| guard.enable_mode(mode))
    }
    /// Restore DEC modes, console modes and both code pages.
    pub fn restore(&mut self) -> io::Result<()> {
        lifecycle::restore(&self.state, true, ConsoleIo::restore)
    }
    /// Read classified VT input. The decoder coalesces ConPTY CR/LF.
    pub fn read_events(&mut self, deadline: Instant) -> io::Result<Vec<InputEvent>> {
        let events = self.decoder.take_events();
        if !events.is_empty() {
            return Ok(events);
        }
        let mut bytes = [0; 1024];
        let count = self.receive(&mut bytes, deadline)?;
        self.decoder
            .feed(bytes.get(..count).ok_or(io::ErrorKind::InvalidData)?)?;
        if count == 0 || Instant::now() >= deadline {
            self.decoder.finish_input()?;
        }
        Ok(self.decoder.take_events())
    }

    fn write_until(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<usize> {
        lifecycle::pending_io(
            &self.state,
            |writer| {
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                writer.cancel_pending();
                let pending =
                    ffi::console_write_start(writer.output.as_raw_handle() as usize, bytes)
                        .map_err(io::Error::other)?;
                writer.pending = Some(pending.clone());
                Ok(pending)
            },
            |pending| {
                #[cfg(test)]
                assert!(
                    self.state.try_lock().is_ok(),
                    "pending output retained state"
                );
                finish_write(&pending, deadline)
            },
        )
    }
}
impl ProbeIo for Tty {
    fn send(&mut self, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        while !bytes.is_empty() {
            let count = self.write_until(bytes, deadline)?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = bytes.get(count..).ok_or(io::ErrorKind::InvalidData)?;
        }
        Ok(())
    }
    fn receive(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
        let timeout = u32::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .map_err(|_| io::ErrorKind::InvalidInput)?;
        // Do not hold terminal state while waiting for input: CTRL_CLOSE must
        // restore immediately even when no keyboard input ever arrives.
        let handle = lifecycle::active(&self.state, |guard| {
            Ok(guard.writer().input.as_raw_handle() as usize)
        })?;
        let input = ffi::console_read(handle, bytes.len(), timeout).map_err(io::Error::other)?;
        let count = input.len();
        bytes
            .get_mut(..count)
            .ok_or(io::ErrorKind::InvalidData)?
            .copy_from_slice(&input);
        Ok(count)
    }
    fn detected(&mut self, caps: &Capabilities) -> io::Result<()> {
        lifecycle::active(&self.state, |guard| guard.detected(caps))
    }
}
impl Write for Tty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_until(bytes, Instant::now() + PROBE_TIMEOUT)
    }
    fn flush(&mut self) -> io::Result<()> {
        lifecycle::active(&self.state, |guard| guard.writer().flush())
    }
}
impl Drop for Tty {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    static REPORT: OnceLock<PathBuf> = OnceLock::new();

    pub(super) fn observe_restored_console(event: u32) {
        if let (Some(path), Some(state)) = (REPORT.get(), CONSOLE.get())
            && let Ok(mut state) = state.lock()
        {
            let io = state.guard.writer();
            if let Ok(actual) = ffi::console_state(
                io.input.as_raw_handle() as usize,
                io.output.as_raw_handle() as usize,
            ) {
                let old = io.original;
                let contents = format!(
                    "{event} {} {} {} {} {} {} {} {}",
                    old.input_mode,
                    old.output_mode,
                    old.input_page,
                    old.output_page,
                    actual.input_mode,
                    actual.output_mode,
                    actual.input_page,
                    actual.output_page
                );
                let _ = std::fs::write(path, contents);
            }
        }
    }

    #[test]
    fn windows_lifecycle_child() -> Result<(), Box<dyn std::error::Error>> {
        let Ok(report) = std::env::var("ACYCLIC_TERMINAL_WINDOWS_REPORT") else {
            return Ok(());
        };
        let path = PathBuf::from(&report);
        assert!(REPORT.set(path.clone()).is_ok());
        let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
        let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        let mut baseline = ffi::console_state(
            input.as_raw_handle() as usize,
            output.as_raw_handle() as usize,
        )?;
        baseline.input_mode |= 1 | 2 | 4;
        baseline.output_mode &= !4;
        baseline.input_page = 437;
        baseline.output_page = 437;
        ffi::console_apply(
            input.as_raw_handle() as usize,
            output.as_raw_handle() as usize,
            baseline,
        )?;
        let mut tty = Tty::open()?;
        assert!(
            matches!(tty.send(b"EXPIRED", Instant::now()), Err(error) if error.kind() == io::ErrorKind::TimedOut)
        );
        let raw = ffi::console_state(
            input.as_raw_handle() as usize,
            output.as_raw_handle() as usize,
        )?;
        assert_eq!(raw.input_mode & (1 | 2 | 4), 0);
        assert_ne!(raw.input_mode & 512, 0);
        assert_ne!(raw.output_mode & 4, 0);
        assert_eq!((raw.input_page, raw.output_page), (65001, 65001));
        // The ConPTY host sends one 100-byte input packet. A zero-capacity
        // receive must consume nothing; the next receive may consume only 24.
        std::fs::write(path.with_extension("input-ready"), b"input")?;
        assert_eq!(
            tty.receive(&mut [], Instant::now() + Duration::from_millis(20))?,
            0
        );
        let mut first = [0; 24];
        let first_count = tty.receive(&mut first, Instant::now() + Duration::from_millis(500))?;
        assert!(first_count > 0 && first_count <= first.len());
        let mut received = first[..first_count].to_vec();
        let deadline = Instant::now() + Duration::from_millis(500);
        while received.len() < 100 && Instant::now() < deadline {
            let mut remaining = [0; 24];
            let count = tty.receive(&mut remaining, deadline)?;
            received.extend_from_slice(&remaining[..count]);
        }
        assert_eq!(received, b"0123456789".repeat(10));
        let blocked = std::env::var("ACYCLIC_TERMINAL_WINDOWS_EVENT").as_deref() == Ok("blocked");
        let payload = vec![b'B'; 1024 * 1024];
        if blocked {
            // The host leaves ConPTY output unread through the whole send.
            // write_until also asserts state is unlocked before every wait.
            for _ in 0..2 {
                let start = Instant::now();
                assert!(
                    matches!(tty.send(&payload, start + Duration::from_millis(50)), Err(error) if error.kind() == io::ErrorKind::TimedOut)
                );
                assert!(start.elapsed() < Duration::from_millis(500));
            }
        }
        std::fs::write(path.with_extension("ready"), b"ready")?;
        if blocked {
            // Windows routes GenerateConsoleCtrlEvent through the same server
            // whose output is blocked. Drain only after the timeout assertion;
            // the real-lock loom model covers close racing the pending wait.
            while !path.with_extension("draining").exists() {
                std::thread::sleep(Duration::from_millis(1));
            }
            ffi::console_break()?;
        }
        if std::env::var("ACYCLIC_TERMINAL_WINDOWS_EVENT").as_deref() == Ok("break") {
            ffi::console_break()?;
        }
        loop {
            std::thread::park();
        }
    }
}

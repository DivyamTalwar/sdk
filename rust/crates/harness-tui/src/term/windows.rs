use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::sync::OnceLock;
use std::time::Instant;

use super::lifecycle::{self, Arc, Mutex, State};
use super::{Capabilities, Decoder, Environment, ProbeIo, ScreenMode, detect};
use crate::key_event::InputEvent;

use acyclic_terminal_console as ffi;

struct ConsoleIo {
    input: File,
    output: File,
    original: ffi::ConsoleState,
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
        self.apply(self.original)
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
        self.output.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
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
        let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        let original = ffi::console_state(
            input.as_raw_handle() as usize,
            output.as_raw_handle() as usize,
        )
        .map_err(io::Error::other)?;
        let state = Arc::new(Mutex::new(State::new(ConsoleIo {
            input,
            output,
            original,
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
        Ok(self.decoder.take_events())
    }
}
impl ProbeIo for Tty {
    fn send(&mut self, bytes: &[u8], _deadline: Instant) -> io::Result<()> {
        self.write_all(bytes)
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
        let input = ffi::console_read(handle, timeout).map_err(io::Error::other)?;
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
        lifecycle::active(&self.state, |guard| guard.writer().write(bytes))
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
        let _tty = Tty::open()?;
        let raw = ffi::console_state(
            input.as_raw_handle() as usize,
            output.as_raw_handle() as usize,
        )?;
        assert_eq!(raw.input_mode & (1 | 2 | 4), 0);
        assert_ne!(raw.input_mode & 512, 0);
        assert_ne!(raw.output_mode & 4, 0);
        assert_eq!((raw.input_page, raw.output_page), (65001, 65001));
        std::fs::write(path.with_extension("ready"), b"ready")?;
        if std::env::var("ACYCLIC_TERMINAL_WINDOWS_EVENT").as_deref() == Ok("break") {
            ffi::console_break()?;
        }
        loop {
            std::thread::park();
        }
    }
}

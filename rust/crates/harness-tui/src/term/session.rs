use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use signal_hook::consts::signal::{SIGCONT, SIGHUP, SIGINT, SIGTERM, SIGTSTP};
use signal_hook::iterator::{Handle, Signals};

use super::lifecycle::{self, Arc, Mutex, State};
use super::tty::RawTty;
use super::{Capabilities, Decoder, Environment, ProbeIo, ScreenMode, detect};
use crate::key_event::{InputEvent, KeyCode, KeyEventKind, KeyModifiers};

/// Process-owned terminal lifecycle. The signal worker restores even while the
/// application is computing or blocked outside terminal I/O. The one state lock
/// serializes every output mutation with restore/close, including on resume.
pub struct Tty {
    state: Arc<Mutex<State<RawTty>>>,
    signals: Handle,
    worker: Option<JoinHandle<()>>,
    decoder: Decoder,
    capabilities: Option<Capabilities>,
}

impl Tty {
    /// Open the controlling terminal, including when stdout is redirected.
    pub fn open() -> io::Result<Self> {
        Self::from_file(OpenOptions::new().read(true).write(true).open("/dev/tty")?)
    }

    /// Install signal handling BEFORE making the terminal raw. A process owns
    /// one terminal lifecycle; callers must not open competing sessions.
    pub fn from_file(file: File) -> io::Result<Self> {
        let mut signals = Signals::new([SIGTERM, SIGHUP, SIGINT, SIGTSTP, SIGCONT])?;
        let handle = signals.handle();
        let state = Arc::new(Mutex::new(State::new(RawTty::from_file(file)?)));
        let worker_state = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("terminal-signals".into())
            .spawn(move || {
                for signal in signals.forever() {
                    if signal == SIGCONT {
                        let _ = lifecycle::resume(&worker_state, RawTty::reapply);
                    } else {
                        // No signal-iterator lock is held while acquiring state.
                        let terminating = signal != SIGTSTP;
                        let _ = lifecycle::restore(&worker_state, terminating, RawTty::restore);
                        let _ = signal_hook::low_level::emulate_default_handler(signal);
                        if !terminating {
                            let _ = lifecycle::resume(&worker_state, RawTty::reapply);
                        }
                    }
                }
            })?;
        Ok(Self {
            state,
            signals: handle,
            worker: Some(worker),
            decoder: Decoder::default(),
            capabilities: None,
        })
    }

    /// Probe once and retain the same whole-stream decoder for subsequent input.
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

    /// Enable modes under the signal worker's restoration journal.
    pub fn setup(&mut self, screen: ScreenMode) -> io::Result<()> {
        lifecycle::active(&self.state, |guard| guard.setup(screen))
    }

    /// Enable an additional queried mode under the same journal.
    pub fn enable_mode(&mut self, mode: u16) -> io::Result<()> {
        lifecycle::active(&self.state, |guard| guard.enable_mode(mode))
    }

    /// Restore DEC modes before termios; retain the desired modes for resume.
    pub fn restore(&mut self) -> io::Result<()> {
        lifecycle::restore(&self.state, false, RawTty::restore)
    }

    /// Restore, suspend with the default SIGTSTP disposition, and reapply on
    /// continuation. Pasted Ctrl+Z never enters this path.
    pub fn suspend(&mut self) -> io::Result<()> {
        self.restore()?;
        signal_hook::low_level::emulate_default_handler(SIGTSTP)?;
        lifecycle::resume(&self.state, RawTty::reapply)
    }

    /// Read only classified events. Late probe replies remain in this decoder.
    pub fn read_events(&mut self, deadline: Instant) -> io::Result<Vec<InputEvent>> {
        let mut events = self.decoder.take_events();
        if events.is_empty() {
            let mut bytes = [0; 1024];
            let count = self.receive(&mut bytes, deadline)?;
            self.decoder
                .feed(bytes.get(..count).ok_or(io::ErrorKind::InvalidData)?)?;
            events = self.decoder.take_events();
        }
        let mut output = Vec::new();
        for event in events {
            if matches!(&event, InputEvent::Key(key) if key.code == KeyCode::Char('z')
                && key.modifiers == KeyModifiers::CONTROL && key.kind == KeyEventKind::Press)
            {
                self.suspend()?;
            } else {
                output.push(event);
            }
        }
        Ok(output)
    }
}

impl ProbeIo for Tty {
    fn send(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()> {
        lifecycle::active(&self.state, |guard| guard.writer().send(bytes, deadline))
    }

    fn receive(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(0);
            }
            // Release state between bounded nonblocking reads. The signal
            // worker never waits for the application's input deadline.
            let result = lifecycle::active(&self.state, |guard| {
                guard
                    .writer()
                    .receive(bytes, deadline.min(now + Duration::from_millis(1)))
            });
            match result {
                Ok(0) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::yield_now(),
                result => return result,
            }
        }
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
        let _ = lifecycle::restore(&self.state, true, RawTty::restore);
        self.signals.close();
        // Join only AFTER releasing state: the worker may be finishing restore.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

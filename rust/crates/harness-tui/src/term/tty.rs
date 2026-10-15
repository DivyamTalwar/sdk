use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcsetattr};

use super::{PROBE_TIMEOUT, ProbeIo};

/// Exclusive controlling-terminal connection with raw mode and descriptor flags
/// restored on drop. Own this outside the output restore guard so DEC modes
/// are undone before the original line discipline is restored.
pub struct Tty {
    file: File,
    original: Termios,
    flags: OFlags,
}

impl Tty {
    /// Use the controlling terminal even when stdout is redirected.
    pub fn open() -> io::Result<Self> {
        Self::from_file(OpenOptions::new().read(true).write(true).open("/dev/tty")?)
    }

    /// Take exclusive ownership of a terminal descriptor (also usable with a
    /// PTY slave). Preserve already-enabled raw mode and nonblocking flags.
    pub fn from_file(file: File) -> io::Result<Self> {
        let original = tcgetattr(&file)?;
        let flags = fcntl_getfl(&file)?;
        let tty = Self {
            file,
            original,
            flags,
        };
        let mut raw = tty.original.clone();
        raw.make_raw();
        // The guard exists before either fallible mutation.
        tcsetattr(&tty.file, OptionalActions::Now, &raw)?;
        fcntl_setfl(&tty.file, flags | OFlags::NONBLOCK)?;
        Ok(tty)
    }

    /// Restore OS terminal state explicitly; drop retries the same operations.
    pub fn restore(&mut self) -> io::Result<()> {
        let termios = tcsetattr(&self.file, OptionalActions::Now, &self.original);
        let flags = fcntl_setfl(&self.file, self.flags);
        termios.and(flags).map_err(Into::into)
    }

    fn write_until(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<usize> {
        loop {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.file.write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => pause(deadline),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl ProbeIo for Tty {
    fn send(&mut self, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.write_until(bytes, deadline)?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = bytes.get(count..).ok_or(io::ErrorKind::InvalidData)?;
        }
        self.flush()
    }

    fn receive(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
        loop {
            if Instant::now() >= deadline {
                return Ok(0);
            }
            match self.file.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => pause(deadline),
                result => return result,
            }
        }
    }
}

impl Write for Tty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_until(bytes, Instant::now() + PROBE_TIMEOUT)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

// Nonblocking reads work on /dev/tty on macOS, where poll(2) is unsupported.
// A single owner and bounded 1 ms waits avoid a reader thread or global lock.
fn pause(deadline: Instant) {
    std::thread::sleep(
        Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
    );
}

impl Drop for Tty {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

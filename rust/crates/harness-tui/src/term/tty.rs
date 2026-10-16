use std::fs::File;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcsetattr};

use super::{PROBE_TIMEOUT, ProbeIo};

/// Exclusive controlling-terminal connection with raw mode and descriptor flags
/// restored on drop. The lifecycle restores its output guard first so DEC
/// modes are undone before the original line discipline is restored.
pub(crate) struct RawTty {
    file: File,
    original: Termios,
    flags: OFlags,
}

impl RawTty {
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
        let termios = tcsetattr(&self.file, OptionalActions::Now, &self.original).and_then(|()| {
            #[cfg(target_os = "macos")]
            if !self
                .original
                .local_modes
                .contains(rustix::termios::LocalModes::PENDIN)
            {
                // Darwin sets PENDIN when ICANON is restored and preserves it
                // across tcsetattr. FIONREAD runs ttypend without consuming
                // input: queued bytes reenter the original line discipline
                // and the synthetic PENDIN bit is cleared (xnu tty.c/ttnread).
                let _ = rustix::io::ioctl_fionread(&self.file)?;
            }
            Ok(())
        });
        let flags = fcntl_setfl(&self.file, self.flags);
        termios.and(flags).map_err(Into::into)
    }

    pub(crate) fn reapply(&mut self) -> io::Result<()> {
        let mut raw = self.original.clone();
        raw.make_raw();
        tcsetattr(&self.file, OptionalActions::Now, &raw)?;
        fcntl_setfl(&self.file, self.flags | OFlags::NONBLOCK)?;
        Ok(())
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

impl ProbeIo for RawTty {
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

impl Write for RawTty {
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

impl Drop for RawTty {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

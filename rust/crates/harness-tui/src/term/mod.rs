//! Query-based terminal capabilities. Keep one detection result for the session.

mod capabilities;
mod probe;
mod restore;
#[cfg(unix)]
mod tty;

pub use capabilities::*;
pub use probe::{PROBE_LIMIT, PROBE_TIMEOUT, Probe, ProbeIo, ProbeReplies, detect};
pub use restore::{RestoreGuard, ScreenMode};
#[cfg(unix)]
pub use tty::Tty;

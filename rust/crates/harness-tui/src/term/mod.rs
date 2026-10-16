//! Query-based terminal capabilities. Keep one detection result for the session.

mod capabilities;
mod decoder;
mod lifecycle;
mod probe;
mod restore;
#[cfg(all(unix, not(acyclic_harness_tui_loom)))]
mod session;
#[cfg(all(unix, not(acyclic_harness_tui_loom)))]
mod signals;
#[cfg(all(unix, not(acyclic_harness_tui_loom)))]
mod tty;

pub use capabilities::*;
pub use decoder::{Decoder, encode_key};
pub use probe::{PROBE_LIMIT, PROBE_TIMEOUT, Probe, ProbeIo, ProbeReplies, detect};
pub use restore::{RestoreGuard, ScreenMode};
#[cfg(all(unix, not(acyclic_harness_tui_loom)))]
pub use session::Tty;
#[cfg(all(windows, not(acyclic_harness_tui_loom)))]
mod windows;
#[cfg(all(windows, not(acyclic_harness_tui_loom)))]
pub use windows::Tty;

//! Query-based terminal capabilities. Keep one detection result for the session.

mod capabilities;
mod decoder;
mod lifecycle;
mod probe;
mod restore;
#[cfg(all(unix, not(feature = "model-checking")))]
mod session;
#[cfg(all(unix, not(feature = "model-checking")))]
mod tty;

pub use capabilities::*;
pub use decoder::{Decoder, encode_key};
pub use probe::{PROBE_LIMIT, PROBE_TIMEOUT, Probe, ProbeIo, ProbeReplies, detect};
pub use restore::{RestoreGuard, ScreenMode};
#[cfg(all(unix, not(feature = "model-checking")))]
pub use session::Tty;
#[cfg(all(windows, not(feature = "model-checking")))]
mod windows;
#[cfg(all(windows, not(feature = "model-checking")))]
pub use windows::Tty;

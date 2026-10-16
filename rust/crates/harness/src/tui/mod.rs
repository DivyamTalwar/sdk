//! Shared widgets and ClientViews presentation for native clients.
pub use acyclic_harness_tui::*;
#[cfg(feature = "terminal")]
mod render;
#[cfg(feature = "terminal")]
pub use render::ClientRenderer;

//! Per-user local transport for the canonical harness wire API.

mod client;
mod endpoint;
mod frames;
mod lifetime;
mod listener;
#[cfg(test)]
mod model;
mod registry;
mod server;
mod sync;

pub use client::LocalHarnessClient;
pub use endpoint::ServiceEndpoint;
pub use server::HarnessService;

fn storage(error: impl std::fmt::Display) -> crate::Error {
    crate::Error::Storage(error.to_string())
}

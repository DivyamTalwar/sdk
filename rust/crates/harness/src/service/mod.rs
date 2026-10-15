//! Per-user local transport for the canonical harness wire API.

mod client;
mod endpoint;
mod frames;
mod server;
#[cfg(test)]
mod tests;

pub use client::LocalHarnessClient;
pub use endpoint::ServiceEndpoint;
pub use server::HarnessService;

fn storage(error: impl std::fmt::Display) -> crate::Error {
    crate::Error::Storage(error.to_string())
}

//! External host hooks normalized into immutable task requests.

mod answer;
mod catalog;
mod client;
mod failure;
mod handler;
mod manifest;
#[cfg(all(feature = "hooks", not(target_arch = "wasm32")))]
mod native;
mod normalize;
mod request;
mod session;
mod volume;

pub use answer::hook_answer;
pub use catalog::{ExternalHost, HookAction, HookSpec, hooks};
pub use failure::hook_failure;
pub use handler::handle;
pub use manifest::{claude_hooks, claude_manifest};
#[cfg(all(feature = "hooks", not(target_arch = "wasm32")))]
pub use native::{HookBinding, run_hook};
pub use normalize::request;
pub use request::{ExternalAction, ExternalRequest};
pub use session::CommandSessionMachine;

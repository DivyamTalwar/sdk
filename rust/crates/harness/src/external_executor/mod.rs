//! External host hooks normalized into immutable task requests.

mod answer;
mod catalog;
mod client;
#[cfg(feature = "filesystem")]
mod commands;
mod failure;
#[cfg(all(feature = "hooks", not(target_arch = "wasm32")))]
mod handoff;
#[cfg(feature = "filesystem")]
mod intake;
mod manifest;
#[cfg(all(feature = "filesystem", feature = "hooks", not(target_arch = "wasm32")))]
mod mcp;
#[cfg(all(feature = "hooks", not(target_arch = "wasm32")))]
mod native;
mod normalize;
#[cfg(feature = "filesystem")]
mod publication;
mod request;
mod result;
mod session;
#[cfg(feature = "filesystem")]
mod tool_input;
mod volume;

pub use answer::hook_answer;
pub use catalog::{ExternalHost, HookAction, HookSpec, hooks};
#[cfg(feature = "filesystem")]
pub use commands::RequestCommands;
pub use failure::hook_failure;
#[cfg(all(feature = "hooks", not(target_arch = "wasm32")))]
pub use handoff::native_tool_request;
#[cfg(all(feature = "hooks", not(target_arch = "wasm32")))]
pub(crate) use handoff::read_result as read_native_result;
#[cfg(feature = "filesystem")]
pub use intake::RequestConsumer;
pub use manifest::{claude_hooks, claude_manifest};
#[cfg(all(feature = "filesystem", feature = "hooks", not(target_arch = "wasm32")))]
pub use mcp::{native_tool_catalog, serve_native_tools};
#[cfg(all(feature = "hooks", not(target_arch = "wasm32")))]
pub use native::{HookBinding, run_hook};
pub use normalize::request;
pub use request::{ExternalAction, ExternalRequest};
pub use result::tool_result;
pub use session::CommandSessionMachine;

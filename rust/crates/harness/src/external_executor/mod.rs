//! External host hooks normalized into immutable task requests.

mod catalog;
mod manifest;
mod normalize;
mod request;
mod volume;

pub use catalog::{ExternalHost, HookAction, HookSpec, hooks};
pub use manifest::{claude_hooks, claude_manifest};
pub use normalize::request;
pub use request::{ExternalAction, ExternalRequest};

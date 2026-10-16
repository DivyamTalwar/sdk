mod error {
    use thiserror::Error;
    include!("../src/lib/error.rs");
}
pub use error::StreamError;
#[path = "../src/change.rs"]
mod change;

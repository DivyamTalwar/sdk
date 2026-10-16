//! Windows console ABI boundary. Terminal policy and synchronization live in
//! harness-tui; this crate only exposes checked native operations through CXX.
#![cfg(windows)]

use std::io;
use std::sync::OnceLock;

#[cxx::bridge(namespace = "acyclic_terminal")]
mod ffi {
    /// Original console modes and UTF-8 translation code pages.
    #[derive(Clone, Copy)]
    struct ConsoleState {
        /// Input processing flags.
        input_mode: u32,
        /// Output processing flags.
        output_mode: u32,
        /// Input encoding page.
        input_page: u32,
        /// Output encoding page.
        output_page: u32,
    }
    unsafe extern "C++" {
        include!("src/windows.h");
        /// Read console modes and encoding pages from validated OS handles.
        fn console_state(input: usize, output: usize) -> Result<ConsoleState>;
        /// Apply every field, attempting all restores even if one fails.
        fn console_apply(input: usize, output: usize, state: ConsoleState) -> Result<()>;
        fn console_register() -> Result<()>;
        /// Generate a real CTRL_BREAK event in the current console.
        fn console_break() -> Result<()>;
        /// Read available VT bytes before the bounded timeout expires.
        fn console_read(input: usize, capacity: usize, timeout: u32) -> Result<Vec<u8>>;
    }
    extern "Rust" {
        fn console_control(event: u32) -> bool;
    }
}

pub use ffi::{ConsoleState, console_apply, console_break, console_read, console_state};

static HANDLER: OnceLock<Box<dyn Fn(u32) -> bool + Send + Sync>> = OnceLock::new();

fn console_control(event: u32) -> bool {
    HANDLER.get().is_some_and(|handler| handler(event))
}

/// Install the synchronous process control handler before changing modes. The
/// OS owns the callback lifetime; it is published once and never replaced.
pub fn console_register(handler: impl Fn(u32) -> bool + Send + Sync + 'static) -> io::Result<()> {
    HANDLER
        .set(Box::new(handler))
        .map_err(|_| io::ErrorKind::AlreadyExists)?;
    ffi::console_register().map_err(io::Error::other)
}

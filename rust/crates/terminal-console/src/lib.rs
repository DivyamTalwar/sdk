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
    #[derive(Clone, Copy)]
    struct ConsoleWriteResult {
        written: u32,
        error: u32,
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
        /// Native operation retaining its owned bytes until OS completion.
        type ConsoleWrite;
        /// Submit one bounded overlapped write without waiting for completion.
        fn console_write_start(output: usize, bytes: &[u8]) -> Result<SharedPtr<ConsoleWrite>>;
        /// Wait outside the lifecycle lock; cancel this operation on timeout.
        fn console_write_wait(write: &SharedPtr<ConsoleWrite>, timeout: u32) -> ConsoleWriteResult;
        /// Fence an operation and cancel both submission and pending driver I/O.
        fn console_write_cancel(write: &SharedPtr<ConsoleWrite>);
        /// Cancel outstanding output before restoring the console.
        fn console_cancel(output: usize) -> Result<()>;
    }
    extern "Rust" {
        fn console_control(event: u32) -> bool;
    }
}

pub use ffi::{ConsoleState, console_apply, console_break, console_read, console_state};
pub use ffi::{
    ConsoleWrite, console_cancel, console_write_cancel, console_write_start, console_write_wait,
};
/// Shared ownership across the bounded waiter and native worker.
pub type PendingWrite = cxx::SharedPtr<ConsoleWrite>;

// SAFETY: native operations own their buffers and OVERLAPPED storage. The
// cancellation flag is atomic; the worker publishes results through its Win32
// completion event before wait reads them. Handle/event fields are immutable
// after publication, and shared_ptr keeps storage alive across cancellation.
unsafe impl Send for ConsoleWrite {}
unsafe impl Sync for ConsoleWrite {}

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

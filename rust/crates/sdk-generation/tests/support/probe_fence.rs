use std::io;
#[path = "../../src/shared_cache.rs"]
pub mod storage;

// The lock is a leaf. Storage callbacks perform one record read/atomic replace;
// a compute callback invokes the concrete executable, never another wrapper.
pub fn query<S: Eq, T, const FENCED: bool>(
    lock: &storage::Lock,
    before: &S,
    observe: impl Fn() -> io::Result<S>,
    read: impl FnOnce() -> io::Result<Option<T>>,
    compute: impl FnOnce() -> io::Result<T>,
    publish: impl FnOnce(&T) -> io::Result<()>,
) -> io::Result<T> {
    let unchanged = || {
        if FENCED && observe()? != *before {
            return Err(io::Error::other("concrete compiler changed during probe"));
        }
        Ok(())
    };
    storage::get_or_compute(
        lock,
        || {
            unchanged()?;
            let value = read()?;
            unchanged()?;
            Ok(value)
        },
        || {
            let value = compute()?;
            unchanged()?;
            Ok(value)
        },
        |value| {
            unchanged()?;
            publish(value)
        },
    )
}

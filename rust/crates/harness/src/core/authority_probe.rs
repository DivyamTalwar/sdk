//! Counts verifications of one caller grant on the current test thread.
use std::cell::RefCell;

thread_local! {
    static ACTIVE: RefCell<Option<(String, usize)>> = const { RefCell::new(None) };
}

pub(crate) fn start(id: &str) {
    ACTIVE.with(|active| *active.borrow_mut() = Some((id.into(), 0)));
}

pub(super) fn record(id: &str) {
    ACTIVE.with(|active| {
        if let Some((expected, count)) = active.borrow_mut().as_mut()
            && expected == id
        {
            *count += 1;
        }
    });
}

pub(crate) fn finish() -> usize {
    ACTIVE.with(|active| active.borrow_mut().take().map_or(0, |(_, count)| count))
}

use super::{AtomicU64, Mutex, Ordering, StreamError};

pub(super) struct Progress {
    // Lock order: this is the only cursor lock. Never nest it with another
    // lock or hold it across an await; all methods return owned scalar values.
    state: Mutex<(u64, u64)>,
}
impl Progress {
    pub(super) fn new(from: u64) -> Self {
        Self {
            state: Mutex::new((from, 1)),
        }
    }
    pub(super) fn next(&self) -> Result<u64, StreamError> {
        Ok(self.state.lock().map_err(|_| StreamError::Unavailable)?.0)
    }
    pub(super) fn fence(&self) -> Result<u64, StreamError> {
        let mut state = self.state.lock().map_err(|_| StreamError::Unavailable)?;
        state.1 = state.1.checked_add(1).ok_or(StreamError::Capacity)?;
        Ok(state.1)
    }
    pub(super) fn consume(&self, generation: u64, sequence: u64) -> Result<bool, StreamError> {
        let mut state = self.state.lock().map_err(|_| StreamError::Unavailable)?;
        if generation != state.1 || sequence < state.0 {
            return Ok(false);
        }
        if sequence > state.0 {
            return Err(StreamError::Unavailable);
        }
        state.0 = state.0.checked_add(1).ok_or(StreamError::OutOfRange)?;
        Ok(true)
    }
}

pub(super) fn allocate(counter: &AtomicU64) -> Result<u64, StreamError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(2)
        })
        .map_err(|_| StreamError::Capacity)
}

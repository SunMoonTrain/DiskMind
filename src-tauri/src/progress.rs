use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

/// Live scan counters shared between the scanner threads and the progress emitter.
#[derive(Default)]
pub struct Counters {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    /// Units of work done / total (MFT records); total == 0 means indeterminate.
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub phase: Mutex<String>,
    pub mode: Mutex<String>,
}

impl Counters {
    pub fn reset(&self) {
        for a in [&self.files, &self.dirs, &self.bytes, &self.done, &self.total] {
            a.store(0, Relaxed);
        }
    }
    pub fn set_phase(&self, p: &str) {
        *self.phase.lock().unwrap() = p.to_string();
    }
    pub fn set_mode(&self, m: &str) {
        *self.mode.lock().unwrap() = m.to_string();
    }
}

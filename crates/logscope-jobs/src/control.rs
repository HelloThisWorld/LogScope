//! Cooperative pause/cancel control shared between a job worker and its owner.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::error::Cancelled;

struct ControlInner {
    cancel_requested: AtomicBool,
    paused: Mutex<bool>,
    unpause: Condvar,
}

/// Cloneable handle for requesting pause, resume, and cancellation.
///
/// Workers call [`JobControl::checkpoint`] at safe points (typically between
/// record batches): it blocks while paused and reports requested
/// cancellation. Cancellation also wakes paused workers so they can exit.
#[derive(Clone)]
pub struct JobControl {
    inner: Arc<ControlInner>,
}

impl Default for JobControl {
    fn default() -> Self {
        Self::new()
    }
}

impl JobControl {
    pub fn new() -> Self {
        JobControl {
            inner: Arc::new(ControlInner {
                cancel_requested: AtomicBool::new(false),
                paused: Mutex::new(false),
                unpause: Condvar::new(),
            }),
        }
    }

    pub fn cancel(&self) {
        self.inner.cancel_requested.store(true, Ordering::SeqCst);
        // Wake a paused worker so it can observe the cancellation.
        let _guard = self.inner.paused.lock();
        self.inner.unpause.notify_all();
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.inner.cancel_requested.load(Ordering::SeqCst)
    }

    pub fn pause(&self) {
        *self.inner.paused.lock() = true;
    }

    pub fn resume(&self) {
        let mut paused = self.inner.paused.lock();
        *paused = false;
        self.inner.unpause.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        *self.inner.paused.lock()
    }

    /// Safe-point check: blocks while paused, then reports cancellation.
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.is_cancel_requested() {
            return Err(Cancelled);
        }
        let mut paused = self.inner.paused.lock();
        while *paused && !self.is_cancel_requested() {
            self.inner.unpause.wait(&mut paused);
        }
        drop(paused);
        if self.is_cancel_requested() {
            return Err(Cancelled);
        }
        Ok(())
    }
}

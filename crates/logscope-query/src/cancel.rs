//! Query cancellation and execution budgets.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use duckdb::InterruptHandle;

/// Cloneable cancel handle for one in-flight query session.
#[derive(Clone)]
pub struct QueryCancelHandle {
    interrupt: Arc<InterruptHandle>,
    cancelled: Arc<AtomicBool>,
}

impl QueryCancelHandle {
    pub fn new(interrupt: Arc<InterruptHandle>) -> Self {
        QueryCancelHandle {
            interrupt,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Requests cancellation: interrupts the engine and marks the session.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.interrupt.interrupt();
    }

    pub fn was_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Runs `f` under an execution budget: if it does not finish within
/// `budget`, the engine is interrupted and the result maps to `Timeout`.
/// External cancellation (via `handle.cancel()`) maps to `Cancelled`.
pub fn run_bounded<T>(
    handle: &QueryCancelHandle,
    budget: Duration,
    f: impl FnOnce() -> Result<T, crate::QueryError>,
) -> Result<T, crate::QueryError> {
    let timed_out = Arc::new(AtomicBool::new(false));
    let stop_watchdog = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let handle = handle.clone();
        let timed_out = timed_out.clone();
        let stop = stop_watchdog.clone();
        std::thread::spawn(move || {
            let step = Duration::from_millis(25);
            let mut waited = Duration::ZERO;
            while waited < budget {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            if !stop.load(Ordering::SeqCst) {
                timed_out.store(true, Ordering::SeqCst);
                handle.interrupt.interrupt();
            }
        })
    };

    let result = f();
    stop_watchdog.store(true, Ordering::SeqCst);
    let _ = watchdog.join();

    match result {
        Err(crate::QueryError::Engine(e)) if crate::QueryError::is_interrupt(&e) => {
            if handle.was_cancelled() {
                Err(crate::QueryError::Cancelled)
            } else if timed_out.load(Ordering::SeqCst) {
                Err(crate::QueryError::Timeout)
            } else {
                Err(crate::QueryError::Engine(e))
            }
        }
        other => other,
    }
}

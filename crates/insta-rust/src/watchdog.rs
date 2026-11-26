//! Execution-time budget timer that triggers async vCPU cancellation on runaway guests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::backend::CancelHandle;

/// Armed watchdog that cancels a running vCPU if `timeout` elapses before `Drop`.
pub struct Timer {
    state: Arc<(Mutex<bool>, Condvar)>,
    fired: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Timer {
    /// Spawns a watchdog thread that invokes `handle.cancel()` after `timeout`.
    pub fn arm(timeout: Duration, handle: Arc<dyn CancelHandle>) -> Self {
        let state = Arc::new((Mutex::new(false), Condvar::new()));
        let fired = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_fired = Arc::clone(&fired);

        let worker = thread::spawn(move || {
            let (lock, cvar) = &*worker_state;
            let mut done = lock.lock().unwrap_or_else(PoisonError::into_inner);
            while !*done {
                let (next, res) = cvar
                    .wait_timeout(done, timeout)
                    .unwrap_or_else(PoisonError::into_inner);
                done = next;
                if !*done && res.timed_out() {
                    worker_fired.store(true, Ordering::Release);
                    handle.cancel();
                    break;
                }
            }
        });

        Self {
            state,
            fired,
            worker: Some(worker),
        }
    }

    /// Returns whether the watchdog timer elapsed and issued a cancel.
    #[allow(dead_code)] // Useful for callers/tests inspecting timeout source directly.
    pub fn has_fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.state;
        let mut done = lock.lock().unwrap_or_else(PoisonError::into_inner);
        *done = true;
        cvar.notify_one();
        drop(done);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

//! The mechanism for stopping or pausing a running script from outside.
//!
//! # Why a shared flag rather than a job
//!
//! The IDE's engine processes jobs serially on a single worker thread. While a
//! script is running that thread does not come back, so **sending "stop" as a
//! job would just queue behind it and never arrive.** This shared flag is the
//! side channel that bypasses the queue.
//!
//! # Where it is checked
//!
//! - between Rhai statements (`on_progress` in `mekiki-scripting`)
//! - in the wait loops ([`crate::Mekiki::wait_actionable`] and friends)
//! - part way through a cursor move ([`crate::Mekiki::move_to`]); it stops where
//!   it is instead of jumping to the target
//!
//! If it exits with an input still held, the caller releases the button. To
//! cover anything missed, an interruption always runs
//! [`crate::Mekiki::release_input`] to clear the held state.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

const RUNNING: u8 = 0;
const PAUSED: u8 = 1;
const STOPPING: u8 = 2;

/// The execution state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    Paused,
    Stopping,
}

/// The shared flag carrying stop and pause requests.
///
/// Clones point at the same state. The UI side and the execution side each hold
/// one.
#[derive(Clone, Debug, Default)]
pub struct Interrupt {
    state: Arc<AtomicU8>,
}

impl Interrupt {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> RunState {
        match self.state.load(Ordering::Relaxed) {
            PAUSED => RunState::Paused,
            STOPPING => RunState::Stopping,
            _ => RunState::Running,
        }
    }

    /// Request a stop. This releases a pause as well and stops.
    pub fn stop(&self) {
        self.state.store(STOPPING, Ordering::Relaxed);
    }

    /// Request a pause. **Does not override an outstanding stop request.**
    /// Once stopping has been decided, it must not become un-stoppable.
    pub fn pause(&self) {
        let _ = self
            .state
            .compare_exchange(RUNNING, PAUSED, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Release a pause. Does not clear a stop request (same reason as above).
    pub fn resume(&self) {
        let _ = self
            .state
            .compare_exchange(PAUSED, RUNNING, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Reset the state ready for the next run.
    pub fn reset(&self) {
        self.state.store(RUNNING, Ordering::Relaxed);
    }

    pub fn is_stopping(&self) -> bool {
        self.state.load(Ordering::Relaxed) == STOPPING
    }

    pub fn is_paused(&self) -> bool {
        self.state.load(Ordering::Relaxed) == PAUSED
    }

    /// While paused, wait until it is released or a stop is requested.
    ///
    /// Returns **how long it waited**. The caller has to subtract that from its
    /// deadline calculation; without doing so, merely pausing would time the
    /// search out.
    pub fn wait_while_paused(&self) -> Duration {
        if !self.is_paused() {
            return Duration::ZERO;
        }
        let started = Instant::now();
        // Polling rather than a condvar: carrying one would cost `Interrupt`
        // its near-`Copy` lightness and make it awkward to touch from the UI.
        // This granularity is responsive enough while paused.
        while self.is_paused() {
            std::thread::sleep(POLL);
        }
        started.elapsed()
    }
}

/// How often the pause is re-checked.
const POLL: Duration = Duration::from_millis(20);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_running() {
        let i = Interrupt::new();
        assert_eq!(i.state(), RunState::Running);
        assert!(!i.is_stopping());
        assert_eq!(i.wait_while_paused(), Duration::ZERO);
    }

    #[test]
    fn clones_share_the_same_state() {
        let a = Interrupt::new();
        let b = a.clone();
        b.stop();
        assert!(a.is_stopping(), "the clone holds a different state");
    }

    /// Once stopping is decided, a pause must not overwrite it.
    #[test]
    fn pause_does_not_override_stop() {
        let i = Interrupt::new();
        i.stop();
        i.pause();
        assert_eq!(i.state(), RunState::Stopping);
        i.resume();
        assert_eq!(i.state(), RunState::Stopping, "resume cleared the stop");
    }

    #[test]
    fn pause_and_resume_round_trip() {
        let i = Interrupt::new();
        i.pause();
        assert!(i.is_paused());
        i.resume();
        assert_eq!(i.state(), RunState::Running);
    }

    /// A paused wait returns the elapsed time once released.
    #[test]
    fn wait_returns_the_paused_duration() {
        let i = Interrupt::new();
        i.pause();

        let other = i.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            other.resume();
        });

        let waited = i.wait_while_paused();
        assert!(
            waited >= Duration::from_millis(40),
            "waited too briefly: {waited:?}"
        );
        assert_eq!(i.state(), RunState::Running);
    }

    /// Requesting a stop while paused breaks out of the wait.
    #[test]
    fn stop_releases_a_paused_wait() {
        let i = Interrupt::new();
        i.pause();

        let other = i.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            other.stop();
        });

        i.wait_while_paused();
        assert!(i.is_stopping());
    }

    #[test]
    fn reset_clears_everything() {
        let i = Interrupt::new();
        i.stop();
        i.reset();
        assert_eq!(i.state(), RunState::Running);
    }
}

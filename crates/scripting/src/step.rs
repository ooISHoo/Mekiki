//! Step execution and line-highlight control.
//!
//! # Built on a volatile API
//!
//! This is implemented with Rhai's `Engine::register_debugger`, which Rhai
//! itself declares as
//!
//! > This API is NOT deprecated, but it is considered volatile and
//! > may change in the future.
//!
//! The `#[deprecated]` attribute is that announcement, not a plan to remove it.
//! Call sites carry `#[allow(deprecated)]`.
//!
//! **Upgrading Rhai may break this.** To catch that, `tests/step.rs` pins the
//! behaviour. If those tests fail after an upgrade, consider rebuilding this
//! mechanism.
//!
//! # How execution is halted
//!
//! A shared flag, on the same principle as stop and pause
//! (`mekiki_core::Interrupt`). The IDE's worker thread does not return while a
//! script runs, so a "next" sent through the job queue would never arrive.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// The polling interval while halted.
const POLL: Duration = Duration::from_millis(20);

/// The value meaning no line number was available.
const NO_LINE: u32 = 0;

#[derive(Debug, Default)]
struct Inner {
    /// Whether to halt at each statement.
    stepping: AtomicBool,
    /// A "next" request. The receiving side resets it to false.
    advance: AtomicBool,
    /// The line currently executing (1 based). 0 means unknown.
    line: AtomicU32,
}

/// Step-execution state. Clones refer to the same state.
#[derive(Clone, Debug, Default)]
pub struct StepControl {
    inner: Arc<Inner>,
}

impl StepControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to halt at each statement. May be toggled mid-run.
    pub fn set_stepping(&self, enabled: bool) {
        self.inner.stepping.store(enabled, Ordering::Relaxed);
        if !enabled {
            // On release, let whatever is halted proceed at once.
            self.inner.advance.store(true, Ordering::Relaxed);
        }
    }

    pub fn is_stepping(&self) -> bool {
        self.inner.stepping.load(Ordering::Relaxed)
    }

    /// Request "next".
    pub fn advance(&self) {
        self.inner.advance.store(true, Ordering::Relaxed);
    }

    /// Consume one "next" request.
    fn take_advance(&self) -> bool {
        self.inner.advance.swap(false, Ordering::Relaxed)
    }

    /// The line currently executing. 0 means unknown.
    pub fn line(&self) -> u32 {
        self.inner.line.load(Ordering::Relaxed)
    }

    fn set_line(&self, line: u32) {
        self.inner.line.store(line, Ordering::Relaxed);
    }

    /// Reset to the pre-run state. Whether stepping is on is the user's setting,
    /// so it is left alone.
    pub fn reset(&self) {
        self.inner.line.store(NO_LINE, Ordering::Relaxed);
        self.inner.advance.store(false, Ordering::Relaxed);
    }

    /// The body called from the debugger callback.
    ///
    /// Records the line and, while stepping, waits for "next" or a stop.
    /// `should_abort` checks for a stop request and is what breaks the wait.
    pub(crate) fn visit(&self, line: Option<u32>, should_abort: &dyn Fn() -> bool) {
        self.set_line(line.unwrap_or(NO_LINE));

        if !self.is_stepping() {
            return;
        }
        // Discard any "next" queued up just before halting. Keeping it would
        // advance one extra statement the moment stepping is switched on.
        self.take_advance();

        while !self.take_advance() {
            if should_abort() {
                return;
            }
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_the_same_state() {
        let a = StepControl::new();
        let b = a.clone();
        b.set_stepping(true);
        assert!(a.is_stepping());
    }

    #[test]
    fn advance_is_consumed_once() {
        let s = StepControl::new();
        s.advance();
        assert!(s.take_advance());
        assert!(!s.take_advance(), "\"next\" was not consumed");
    }

    /// Releasing stepping lets whatever is halted proceed.
    #[test]
    fn disabling_stepping_releases_the_wait() {
        let s = StepControl::new();
        s.set_stepping(true);
        s.set_stepping(false);
        assert!(s.take_advance(), "still cannot proceed after release");
    }

    #[test]
    fn reset_clears_line_and_pending_advance() {
        let s = StepControl::new();
        s.set_line(42);
        s.advance();
        s.reset();
        assert_eq!(s.line(), 0);
        assert!(!s.take_advance());
    }

    /// Passes straight through when not stepping.
    #[test]
    fn visit_passes_through_when_not_stepping() {
        let s = StepControl::new();
        s.visit(Some(7), &|| false);
        assert_eq!(s.line(), 7);
    }

    /// While stepping, it halts until "next" arrives.
    #[test]
    fn visit_waits_for_advance_while_stepping() {
        let s = StepControl::new();
        s.set_stepping(true);

        let other = s.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            other.advance();
        });

        let started = std::time::Instant::now();
        s.visit(Some(3), &|| false);
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "it did not halt"
        );
        assert_eq!(s.line(), 3);
    }

    /// A stop request breaks the wait. Without that there would be no way to
    /// stop it.
    #[test]
    fn visit_gives_up_when_aborting() {
        let s = StepControl::new();
        s.set_stepping(true);
        s.visit(Some(1), &|| true);
    }
}

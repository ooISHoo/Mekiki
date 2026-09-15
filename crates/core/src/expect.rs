//! Assertions with retries.
//!
//! The implementation of the [Rhai API wait contract](../../../docs/architecture/rhai-api.md#waiting-and-assertions).
//!
//! What separates these from the waiting in [`crate::Act`] is **the statement
//! of intent**.
//!
//! - `wait` = "wait until it appears, then carry on"
//! - `expect` = "it should be there; if it is not, report a failure"
//!
//! A failure message carries the values actually observed, plus the path to the
//! artifacts ([`crate::FailureArtifacts`]).

use std::time::{Duration, Instant};

use crate::artifacts::FailureArtifacts;
use crate::model::Match;
use crate::target::Target;
use crate::{Error, Mekiki, Result};

/// An assertion failure.
#[derive(Debug)]
pub struct AssertionFailed {
    pub message: String,
    pub artifacts: Option<FailureArtifacts>,
}

impl std::fmt::Display for AssertionFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(a) = &self.artifacts {
            write!(f, "\n  diagnostic files: {a}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AssertionFailed {}

/// The temporary object returned by [`Mekiki::expect`].
pub struct Expect<'a> {
    pub(crate) engine: &'a mut Mekiki,
    pub(crate) target: Target,
}

impl Expect<'_> {
    fn timeout(&self, override_: Option<Duration>) -> Duration {
        override_
            .or(self.target.timeout)
            .unwrap_or(self.engine.settings.auto_wait_timeout)
    }

    /// Expect it to appear. Goes through the auto-wait, stability check included.
    pub fn to_appear(mut self, timeout: Option<Duration>) -> Result<Match> {
        if let Some(t) = timeout {
            self.target = self.target.clone().timeout(t);
        }
        self.engine.wait_actionable(&self.target)
    }

    /// Expect it to disappear.
    pub fn to_vanish(self, timeout: Option<Duration>) -> Result<()> {
        let limit = self.timeout(timeout);
        let Expect { engine, target } = self;
        let started = Instant::now();

        loop {
            let (matches, _) = engine.scan_target(&target)?;
            if matches.is_empty() {
                return Ok(());
            }
            if started.elapsed() >= limit {
                let best = matches
                    .first()
                    .map(|m| format!("still at {} with score {:.4}", m.rect, m.score))
                    .unwrap_or_default();
                let artifacts = engine.write_failure_artifacts(&target);
                return Err(Error::Assertion(Box::new(AssertionFailed {
                    message: format!(
                        "'{}' did not disappear after {:.1}s ({best})",
                        target.describe(),
                        started.elapsed().as_secs_f32()
                    ),
                    artifacts,
                })));
            }
            std::thread::sleep(engine.settings.wait_scan_interval);
        }
    }

    /// Expect exactly `expected` matches.
    ///
    /// It waits for the count to settle, so it can ride out a partially drawn
    /// screen where the number is still climbing.
    pub fn to_have_count(self, expected: usize, timeout: Option<Duration>) -> Result<Vec<Match>> {
        let limit = self.timeout(timeout);
        let Expect { engine, target } = self;
        let started = Instant::now();

        loop {
            let (matches, _) = engine.scan_target_all(&target)?;
            let last_seen = matches.len();
            if last_seen == expected {
                return Ok(matches);
            }
            if started.elapsed() >= limit {
                let artifacts = engine.write_failure_artifacts(&target);
                return Err(Error::Assertion(Box::new(AssertionFailed {
                    message: format!(
                        "'{}' should have {expected} matches but has {last_seen} (waited {:.1}s)",
                        target.describe(),
                        started.elapsed().as_secs_f32()
                    ),
                    artifacts,
                })));
            }
            std::thread::sleep(engine.settings.wait_scan_interval);
        }
    }

    /// Expect at least `minimum` matches.
    pub fn to_have_count_at_least(
        self,
        minimum: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<Match>> {
        let limit = self.timeout(timeout);
        let Expect { engine, target } = self;
        let started = Instant::now();

        loop {
            let (matches, _) = engine.scan_target_all(&target)?;
            if matches.len() >= minimum {
                return Ok(matches);
            }
            if started.elapsed() >= limit {
                let seen = matches.len();
                let artifacts = engine.write_failure_artifacts(&target);
                return Err(Error::Assertion(Box::new(AssertionFailed {
                    message: format!(
                        "'{}' should have at least {minimum} matches but has {seen} (waited {:.1}s)",
                        target.describe(),
                        started.elapsed().as_secs_f32()
                    ),
                    artifacts,
                })));
            }
            std::thread::sleep(engine.settings.wait_scan_interval);
        }
    }

    /// Expect at most `maximum` matches.
    ///
    /// The mirror of [`Self::to_have_count_at_least`]: success is the count
    /// settling at or below the bound, so it can ride out extra rows that are
    /// still on their way out.
    pub fn to_have_count_at_most(
        self,
        maximum: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<Match>> {
        let limit = self.timeout(timeout);
        let Expect { engine, target } = self;
        let started = Instant::now();

        loop {
            let (matches, _) = engine.scan_target_all(&target)?;
            if matches.len() <= maximum {
                return Ok(matches);
            }
            if started.elapsed() >= limit {
                let seen = matches.len();
                let artifacts = engine.write_failure_artifacts(&target);
                return Err(Error::Assertion(Box::new(AssertionFailed {
                    message: format!(
                        "'{}' should have at most {maximum} matches but has {seen} (waited {:.1}s)",
                        target.describe(),
                        started.elapsed().as_secs_f32()
                    ),
                    artifacts,
                })));
            }
            std::thread::sleep(engine.settings.wait_scan_interval);
        }
    }
}

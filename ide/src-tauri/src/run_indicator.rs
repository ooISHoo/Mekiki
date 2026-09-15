//! Reflects the script execution state on the OS taskbar button.
//!
//! # Why
//!
//! While a script runs, the IDE window is usually behind the application being
//! automated, so the in-window Run/Stop buttons are not visible. The taskbar
//! button (dock icon on macOS) is the one piece of IDE chrome that stays in
//! view, so it is the natural place to answer "is it still running?".
//!
//! # Boundary
//!
//! This module is the only place that knows *how* a state is drawn by the OS.
//! The frontend and the command layer speak in [`RunIndicator`] terms. A
//! platform-specific presentation added later (a Windows overlay badge, a
//! macOS dock badge label, a Linux Unity badge count) belongs in [`apply`] and
//! must not leak into `main.js` or the command signature.
//!
//! # Current presentation
//!
//! Tauri's taskbar progress bar, without a progress value. Per platform:
//!
//! - **Windows**: `Indeterminate` draws a moving green band, `Paused` a yellow
//!   band, `None` clears it (ITaskbarList3).
//! - **macOS / Linux (Unity)**: the bar is app-wide, and `Indeterminate` /
//!   `Paused` are drawn as a plain `Normal` bar. It still reads as "busy", but a
//!   dock badge would be more idiomatic. Add it here when those targets are
//!   supported.
//! - **iOS / Android**: unsupported; the call is a no-op.

use serde::Deserialize;
use tauri::Runtime;
use tauri::window::{ProgressBarState, ProgressBarStatus};

/// Execution state as the frontend sees it.
///
/// The variant names are the wire format used by `setRunState` in `main.js`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunIndicator {
    Idle,
    Running,
    Paused,
    /// A stop was requested and the script has not returned yet. Presented
    /// like `Running` because the worker may still be finishing a statement.
    Stopping,
}

impl RunIndicator {
    /// Presentation on the taskbar progress bar.
    fn progress_status(self) -> ProgressBarStatus {
        match self {
            RunIndicator::Idle => ProgressBarStatus::None,
            RunIndicator::Running | RunIndicator::Stopping => ProgressBarStatus::Indeterminate,
            RunIndicator::Paused => ProgressBarStatus::Paused,
        }
    }
}

/// Draw `state` on the taskbar button of `window`.
///
/// Failure only affects the indicator, never the script, so callers should log
/// and continue rather than abort the run.
pub fn apply<R: Runtime>(window: &tauri::Window<R>, state: RunIndicator) -> tauri::Result<()> {
    window.set_progress_bar(ProgressBarState {
        status: Some(state.progress_status()),
        progress: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_state_names_deserialize() {
        for (wire, expected) in [
            ("idle", RunIndicator::Idle),
            ("running", RunIndicator::Running),
            ("paused", RunIndicator::Paused),
            ("stopping", RunIndicator::Stopping),
        ] {
            let parsed: RunIndicator = serde_json::from_value(serde_json::json!(wire)).unwrap();
            assert_eq!(parsed, expected, "{wire}");
        }
    }

    #[test]
    fn idle_clears_and_others_show_activity() {
        assert!(matches!(
            RunIndicator::Idle.progress_status(),
            ProgressBarStatus::None
        ));
        assert!(matches!(
            RunIndicator::Running.progress_status(),
            ProgressBarStatus::Indeterminate
        ));
        assert!(matches!(
            RunIndicator::Stopping.progress_status(),
            ProgressBarStatus::Indeterminate
        ));
        assert!(matches!(
            RunIndicator::Paused.progress_status(),
            ProgressBarStatus::Paused
        ));
    }
}

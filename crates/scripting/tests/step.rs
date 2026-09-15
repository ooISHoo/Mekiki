//! Pins the behaviour of step execution and line highlighting.
//!
//! # If this test fails
//!
//! It rests on Rhai's `Engine::register_debugger`, which Rhai itself declares as
//!
//! > This API is NOT deprecated, but it is considered volatile and
//! > may change in the future.
//!
//! a **volatile API**. The `#[deprecated]` attribute is that announcement, not
//! a plan to remove it. Call sites carry `#[allow(deprecated)]`.
//!
//! **If this fails after a Rhai upgrade, that is the signal of a behaviour
//! change.** Before fixing it, check the following.
//!
//! - Whether the variants and meaning of `DebuggerCommand` changed
//! - Whether the callback now fires after returning `Continue` (today it stops,
//!   which is why `StepInto` is returned for line highlighting)
//! - Whether the line numbers in the `Position` handed to the callback are still
//!   1 based
//!
//! Do not simply delete the test. Its failing is the only detection there is.

#![cfg(feature = "debugging")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mekiki_core::{Interrupt, Mekiki, Settings};
use mekiki_scripting::assets::AssetStore;
use mekiki_scripting::step::StepControl;
use mekiki_scripting::{ScriptHost, api::Runtime};

/// A host needing neither screen nor mouse. All that is measured here is how
/// execution progresses.
fn host(dir: &std::path::Path) -> ScriptHost {
    let mekiki = Mekiki::with_settings(Settings {
        artifact_dir: None,
        ..Default::default()
    })
    .expect("cannot create the engine");
    ScriptHost::from_runtime(Runtime::new(mekiki, AssetStore::new(dir)))
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mekiki-step-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Line numbers are available and advance with execution. The foundation of
/// line highlighting.
///
/// **Observation happens while halted at a step.** Run plainly, the script is
/// too fast to sample from outside, and failing to sample would be
/// indistinguishable from being broken.
///
/// `ScriptHost` holds an `Rc` so it cannot cross threads. The script runs on
/// this thread while observation and "next" come from another.
#[test]
fn reports_the_current_line() {
    let dir = temp_dir("line");
    let mut h = host(&dir);
    let step = StepControl::new();
    h.set_debugger(step.clone(), Interrupt::new());
    step.set_stepping(true);

    let seen = Arc::new(Mutex::new(Vec::<u32>::new()));
    let watcher = seen.clone();
    let driver = step.clone();

    let handle = std::thread::spawn(move || {
        // Read while it should be halted, then advance one statement, repeatedly.
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(15));
            let l = driver.line();
            if l > 0 {
                let mut v = watcher.lock().unwrap();
                if v.last() != Some(&l) {
                    v.push(l);
                }
            }
            driver.advance();
        }
        // Release at the end so it finishes even if a step was missed.
        driver.set_stepping(false);
    });

    h.run("let a = 1;\nlet b = 2;\nlet c = a + b;\n").unwrap();
    handle.join().unwrap();

    let lines = seen.lock().unwrap().clone();
    assert!(!lines.is_empty(), "no line number was ever observed");
    assert!(
        lines.iter().all(|l| (1..=3).contains(l)),
        "line number out of range; it may no longer be 1 based: {lines:?}"
    );
    assert!(
        lines.windows(2).all(|w| w[0] <= w[1]),
        "the line went backwards; it does not follow execution order: {lines:?}"
    );
    assert!(
        lines.len() >= 2,
        "only one line was observed; the callback may not fire per statement: {lines:?}"
    );
}

/// While stepping, nothing advances until "next" is sent.
#[test]
fn stepping_waits_for_advance() {
    let dir = temp_dir("wait");
    let mut h = host(&dir);
    let step = StepControl::new();
    h.set_debugger(step.clone(), Interrupt::new());
    step.set_stepping(true);

    // Send "next" at a fixed interval. It should not advance faster than that.
    let driver = step.clone();
    let stop = Arc::new(Mutex::new(false));
    let stop_flag = stop.clone();
    let handle = std::thread::spawn(move || {
        while !*stop_flag.lock().unwrap() {
            driver.advance();
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    let started = Instant::now();
    h.run("let a = 1;\nlet b = 2;\nlet c = 3;\n").unwrap();
    let elapsed = started.elapsed();

    *stop.lock().unwrap() = true;
    handle.join().unwrap();

    assert!(
        elapsed >= Duration::from_millis(20),
        "it did not halt at a step: {elapsed:?}"
    );
}

/// Releasing stepping lets it run to completion.
#[test]
fn disabling_stepping_lets_it_finish() {
    let dir = temp_dir("release");
    let mut h = host(&dir);
    let step = StepControl::new();
    h.set_debugger(step.clone(), Interrupt::new());
    step.set_stepping(true);

    let releaser = step.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        releaser.set_stepping(false);
    });

    let started = Instant::now();
    h.run("let a = 1;\nlet b = 2;\n").unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it does not finish even after release"
    );
}

/// It can be stopped even while halted at a step.
///
/// **Without this, entering step execution would leave closing the IDE as the
/// only way out.**
#[test]
fn stop_escapes_a_step_pause() {
    let dir = temp_dir("stop");
    let mut h = host(&dir);
    let step = StepControl::new();
    let interrupt = Interrupt::new();
    h.set_debugger(step.clone(), interrupt.clone());
    h.set_interrupt(interrupt.clone());
    step.set_stepping(true);

    let stopper = interrupt.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        stopper.stop();
    });

    let started = Instant::now();
    let _ = h.run("let a = 1;\nlet b = 2;\nlet c = 3;\n");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a stop does not break out of the step wait"
    );
    assert!(interrupt.is_stopping());
}

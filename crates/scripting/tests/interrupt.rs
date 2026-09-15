//! Can a running script actually be stopped?
//!
//! `tests/script.rs` covers stopping a `sleep`, which is Mekiki's own chunked
//! wait and checks the flag itself. That leaves the harder case untested: a
//! script that **never calls into Mekiki at all**. Nothing in the engine can
//! notice that one, so the only thing that can end it is Rhai's `on_progress`.
//!
//! The first agent test round found a runaway loop of exactly this shape
//! burning a core for five minutes with the timeout, the `stop` tool and the
//! emergency hotkey all unable to touch it. These tests pin the layer that has
//! to work for any of those to mean anything.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mekiki_core::Interrupt;
use mekiki_scripting::ScriptHost;

/// Raise the flag once the script has had time to get going.
fn stop_after(interrupt: Interrupt, delay: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        interrupt.stop();
    })
}

/// A pure-compute loop has to be interruptible.
///
/// No engine call, no sleep — just Rhai burning operations. If this hangs, every
/// defence built on `Interrupt` is decorative.
#[test]
fn a_pure_compute_loop_can_be_stopped() {
    let dir = std::env::temp_dir().join("mekiki-interrupt-test");
    std::fs::create_dir_all(&dir).unwrap();

    let mut host = ScriptHost::new(&dir).expect("cannot build a script host");
    let interrupt = Interrupt::new();
    host.set_interrupt(interrupt.clone());

    // A watchdog that fails the test loudly rather than letting it hang forever.
    let finished = Arc::new(AtomicBool::new(false));
    let watch = finished.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(30));
        assert!(
            watch.load(Ordering::SeqCst),
            "the script never stopped: on_progress is not reaching the interrupt"
        );
    });

    let stopper = stop_after(interrupt.clone(), Duration::from_millis(300));

    let started = Instant::now();
    let result = host.run("let x = 0; loop { x += 1; }");
    let elapsed = started.elapsed();
    finished.store(true, Ordering::SeqCst);
    stopper.join().unwrap();

    assert!(
        elapsed < Duration::from_secs(20),
        "the loop ran for {elapsed:?} after the stop was raised"
    );
    assert!(
        result.is_err(),
        "a stopped script should not report success"
    );
    assert!(
        ScriptHost::is_interrupted(&result.unwrap_err()),
        "the failure should be reported as an interruption, not an error"
    );
}

/// The same, for a loop that does call into Rhai functions.
///
/// Function calls take a different path through the interpreter than a bare
/// expression, so a mechanism that only counts one of them would pass the test
/// above and still hang here.
#[test]
fn a_loop_calling_functions_can_be_stopped() {
    let dir = std::env::temp_dir().join("mekiki-interrupt-test-fn");
    std::fs::create_dir_all(&dir).unwrap();

    let mut host = ScriptHost::new(&dir).expect("cannot build a script host");
    let interrupt = Interrupt::new();
    host.set_interrupt(interrupt.clone());

    let stopper = stop_after(interrupt, Duration::from_millis(300));

    let started = Instant::now();
    let result = host.run(
        r#"
        fn work(n) { n * 2 + 1 }
        let total = 0;
        loop { total += work(total) % 7; }
        "#,
    );
    let elapsed = started.elapsed();
    stopper.join().unwrap();

    assert!(
        elapsed < Duration::from_secs(20),
        "the loop ran for {elapsed:?} after the stop was raised"
    );
    assert!(result.is_err());
}

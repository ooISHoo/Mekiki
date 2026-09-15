//! Measures how much Rhai's `debugging` feature costs in execution speed.
//!
//! ```text
//! cargo run --release -p mekiki-scripting --example script_bench
//! cargo run --release -p mekiki-scripting --example script_bench --features debugging
//! ```
//!
//! # The question
//!
//! Step execution and line highlighting need Rhai's `debugging` feature. This
//! measurement decides **whether it can be left on all the time**.
//!
//! Only Rhai's interpretation is affected; image matching is not. So the
//! interpretation cost is measured three ways, to see whether it is negligible
//! in a real script where one action takes tens of milliseconds.
//!
//! - **Arithmetic loop** — raw interpretation speed, where debugging shows most
//! - **Function calls** — closer to an RPA script
//! - **With actions** — assuming 1ms per instruction, close to the real ratio
//!
//! Under `--features debugging`, three variants are measured.
//!
//! - **Unregistered** — the feature is merely enabled
//! - **Continue** — registered, but returns to free running after the first call
//! - **Per statement** — returning `StepInto` so it fires at every node
//!
//! **That distinction matters.** Returning `DebuggerCommand::Continue` moves
//! Rhai's debugger state to `CONTINUE`, after which the callback stops being
//! called (see `dbg` in `rhai/src/eval/debugger.rs`). So Continue makes line
//! highlighting impossible: highlighting requires re-entering at every
//! statement, and that cost is a different thing entirely.

use std::time::{Duration, Instant};

use rhai::Engine;

/// Repetitions per case. Too many drags the measurement out, so keep it modest.
const REPEAT: usize = 5;

fn main() {
    println!("Rhai {} / profile: release", rhai_version());
    #[cfg(feature = "debugging")]
    println!("debugging: on");
    #[cfg(not(feature = "debugging"))]
    println!("debugging: off (the default)");
    println!();

    let cases: &[(&str, &str)] = &[
        (
            "arithmetic loop, 200k iterations",
            r#"
            let total = 0;
            for i in 0..200000 {
                total += i % 7;
            }
            total
            "#,
        ),
        (
            "function calls, 50k iterations",
            r#"
            fn step(x) { x * 2 + 1 }
            let total = 0;
            for i in 0..50000 {
                total += step(i) % 5;
            }
            total
            "#,
        ),
        (
            "with actions, 300 iterations (1ms per instruction)",
            r#"
            let total = 0;
            for i in 0..300 {
                act();
                total += i % 3;
            }
            total
            "#,
        ),
    ];

    println!(
        "{:<34} {:>10} {:>10} {:>10}",
        "case", "unreg.[ms]", "Continue", "per stmt"
    );
    println!("{}", "-".repeat(68));

    for (name, script) in cases {
        let plain = measure(script, Mode::None);

        #[cfg(feature = "debugging")]
        let (cont, every) = (
            format!("{:.1}", measure(script, Mode::Continue)),
            format!("{:.1}", measure(script, Mode::EveryNode)),
        );
        #[cfg(not(feature = "debugging"))]
        let (cont, every) = ("-".to_string(), "-".to_string());

        println!("{name:<34} {plain:>10.1} {cont:>10} {every:>10}");
    }

    println!();
    println!(
        "unregistered = the feature merely enabled; the gap from a disabled build is the feature's own cost."
    );
    println!(
        "Continue = returns to free running after the first call; the callback never fires again."
    );
    println!("per stmt = keeps returning StepInto. **The state line highlighting needs.**");
}

/// How the debugger is attached.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    /// Not registered.
    None,
    /// Registered, but returns to free running after the first call.
    Continue,
    /// Keeps firing at every node, as line highlighting does.
    EveryNode,
}

/// Run the script `REPEAT` times and return the median in milliseconds.
fn measure(script: &str, mode: Mode) -> f64 {
    let mut engine = Engine::new();

    // A stand-in for an action costing 1ms per instruction. A real `click()` is
    // heavier still once the automatic wait and repaint waits are included.
    engine.register_fn("act", || {
        let until = Instant::now() + Duration::from_millis(1);
        while Instant::now() < until {
            std::hint::spin_loop();
        }
    });

    if mode != Mode::None {
        attach(&mut engine, mode);
    }

    let ast = engine
        .compile(script)
        .expect("failed to compile the script");

    let mut times = Vec::with_capacity(REPEAT);
    for _ in 0..REPEAT {
        let t0 = Instant::now();
        let value = engine
            .eval_ast::<rhai::Dynamic>(&ast)
            .expect("failed to run the script");
        // Discarding the result risks the optimiser removing the computation, so
        // touch it.
        let _ = std::hint::black_box(value);
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

/// Attach the debugger, passing straight through without halting.
#[cfg(feature = "debugging")]
fn attach(engine: &mut Engine, mode: Mode) {
    use rhai::debugger::DebuggerCommand;

    let command = match mode {
        // Returns to free running after one call; the callback never fires again.
        Mode::Continue => DebuggerCommand::Continue,
        // Keeps firing at every node. This is what line highlighting needs.
        Mode::EveryNode => DebuggerCommand::StepInto,
        Mode::None => unreachable!("rejected by the caller"),
    };

    // register_debugger is an API Rhai itself declares volatile. The deprecated
    // attribute is that announcement, not a plan to remove it.
    #[allow(deprecated)]
    engine.register_debugger(
        |_engine, debugger| debugger,
        move |_ctx, _event, _node, _source, _pos| Ok(command),
    );
}

#[cfg(not(feature = "debugging"))]
fn attach(_engine: &mut Engine, _mode: Mode) {
    unreachable!("never called in a build without debugging");
}

fn rhai_version() -> &'static str {
    option_env!("CARGO_PKG_VERSION_RHAI").unwrap_or("1.25.1")
}

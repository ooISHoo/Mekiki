//! Diagnostic probe that runs the IDE backend path without Tauri.
//!
//! ```text
//! cargo run --release -p mekiki-ide --example worker_probe
//! ```
//!
//! # What this isolates
//!
//! A script failing to run from the IDE can originate in one of three layers:
//!
//! 1. Engine and scripting, which `mekiki run` can test.
//! 2. **The IDE worker**, including its queue, interrupts, and debugger setup.
//!    This probe covers that layer.
//! 3. Tauri IPC and the frontend, inspected through developer tools.
//!
//! If this probe passes, layers 1 and 2 are healthy and investigation can focus
//! on layer 3.

use std::time::{Duration, Instant};

use mekiki_ide_lib::engine::EngineHandle;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let dir = std::env::temp_dir().join("mekiki-worker-probe");
    std::fs::create_dir_all(&dir).expect("failed to create the working directory");

    let engine = EngineHandle::spawn();

    // --- 1) Verify a round trip through the job queue with inexpensive work. ---
    let t0 = Instant::now();
    match engine.list_windows() {
        Ok(list) => println!("Listed {} windows in {:?}", list.len(), t0.elapsed()),
        Err(e) => {
            println!("Window listing failed: {e}");
            println!("The worker thread may not have started.");
            return;
        }
    }

    // --- 2) Verify script execution. ---
    println!();
    println!("Running script...");
    let t1 = Instant::now();
    let result = engine.run_script(
        dir.clone(),
        r#"
        print("first line");
        let n = window_titles().len();
        print(`visible windows: ${n}`);
        print("last line");
        "#
        .to_string(),
    );

    match result {
        Ok(r) => {
            println!(
                "Result: ok={} interrupted={} {}ms",
                r.ok, r.interrupted, r.elapsed_ms
            );
            println!("Output lines: {}", r.output.len());
            for line in &r.output {
                println!("  {line}");
            }
            if let Some(e) = &r.error {
                println!("Error: {e}");
            }
            if r.output.is_empty() {
                println!();
                println!("**No output.** The print calls were not captured.");
                println!("Check capture_output wiring and debugger registration side effects.");
            }
        }
        Err(e) => {
            println!("Script round trip failed: {e}");
            println!("The worker may be blocked waiting for the debugger.");
        }
    }
    println!("Round trip: {:?}", t1.elapsed());

    // --- 3) Verify the queue remains responsive after execution. ---
    //
    // If execution remains blocked, every later job waits behind it. A slow or
    // missing response here means the worker did not leave the script.
    println!();
    let t2 = Instant::now();
    match engine.list_windows() {
        Ok(_) => println!("Post-run job round trip: {:?}", t2.elapsed()),
        Err(e) => println!("No response after execution: {e}"),
    }

    // --- 4) Verify interruption. ---
    println!();
    println!("Interruption check: stop a 3-second sleep after 0.3 seconds...");
    let stopper = std::thread::spawn({
        move || {
            std::thread::sleep(Duration::from_millis(300));
        }
    });
    stopper.join().ok();
}

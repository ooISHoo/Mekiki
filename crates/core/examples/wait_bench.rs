//! Measures the effect of change detection (Phase 4-2 of the development plan).
//!
//! ```text
//! cargo run --release -p mekiki-core --example wait_bench
//! ```
//!
//! Waits for something that does not exist against a static screen. That is the
//! worst case for `wait`: it polls until the timeout. With change detection
//! working, only the first poll performs a search.
//!
//! It never touches the screen, so neither the mouse nor the keyboard moves.

use std::time::{Duration, Instant};

use mekiki_core::{ChangeDetection, Mekiki, Settings};

const TIMEOUT: Duration = Duration::from_secs(3);

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Err(e) = run() {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Waiting {:.0}s for something absent against a static screen.",
        TIMEOUT.as_secs_f32()
    );
    println!("With change detection working, only the first poll searches.");
    println!();
    println!(
        "{:<10} {:<22} {:>12} {:>12}",
        "detection", "target", "elapsed[ms]", "overshoot[ms]"
    );
    println!("{}", "-".repeat(60));

    for enabled in [false, true] {
        for kind in ["image", "ocr"] {
            let elapsed = measure(enabled, kind)?;
            println!(
                "{:<10} {:<22} {:>12.0} {:>12.0}",
                if enabled { "on" } else { "off" },
                kind,
                elapsed.as_secs_f64() * 1000.0,
                (elapsed.as_secs_f64() - TIMEOUT.as_secs_f64()) * 1000.0,
            );
        }
    }

    println!();
    println!("\"overshoot\" is how many ms past the timeout the run took.");
    println!("Polling waits for a search that straddles the cut-off to finish,");
    println!(
        "so the heavier one search is, the larger the overshoot. Shrinking it means this is working."
    );

    Ok(())
}

fn measure(change_detection: bool, kind: &str) -> Result<Duration, Box<dyn std::error::Error>> {
    let settings = Settings {
        artifact_dir: None,
        auto_wait_timeout: TIMEOUT,
        change_detection: if change_detection {
            ChangeDetection::default()
        } else {
            ChangeDetection::disabled()
        },
        ..Default::default()
    };

    let mut mekiki = Mekiki::with_settings(settings)?;
    let screen = mekiki.primary_screen()?;

    let target = match kind {
        "ocr" => mekiki
            .text_target(screen, "this string should not be on screen_ZZQX")
            .similar(0.95),
        _ => {
            // A pattern that is not on screen, generated deterministically.
            let mut state = 0x00C0_FFEEu32;
            let luma: Vec<u8> = (0..96 * 96)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    (state % 256) as u8
                })
                .collect();
            let pattern = mekiki
                .pattern_from_luma8(&luma, 96, 96, "absent")
                .similar(0.95);
            mekiki.target(screen, &pattern)
        }
    };

    let started = Instant::now();
    let _ = mekiki.on(&target).resolve();
    Ok(started.elapsed())
}

//! Exercises the API ([Rhai API architecture](../../../docs/architecture/rhai-api.md)) on real
//! hardware.
//!
//! It runs the following, all kept within what cannot disturb the screen.
//!
//! 1. Window enumeration and window scoping (G)
//! 2. Lazy `Target` resolution and the automatic wait (A / B)
//! 3. Drawing an outline with `highlight()` (J)
//! 4. `expect().to_vanish()` (E)
//! 5. Searching for a pattern that deliberately cannot be found, to produce
//!    failure artifacts (F)
//!
//! ```text
//! cargo run --release -p mekiki-core --example api_demo
//! ```
//!
//! It never clicks and never moves the mouse.

use std::time::{Duration, Instant};

use mekiki_capture::Rect;
use mekiki_core::{HighlightColor, Mekiki};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Err(e) = run() {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut mekiki = Mekiki::new()?;
    println!("capture: {}", mekiki.capture_diagnostics());
    println!("matching: {}", mekiki.matcher_backend());
    println!();

    // --- 1. window enumeration ---
    println!("=== window enumeration (Z order) ===");
    let windows = mekiki.window_list()?;
    for w in windows.iter().take(8) {
        println!("  [{}] {} {}", w.z_order, w.bounds, truncate(&w.title, 50));
    }
    println!("  ... {} in total", windows.len());
    println!();

    // Pick the window to use as the search area.
    //
    // Windows covering the whole screen (shell overlays and the like) are
    // excluded: scoping to them is pointless, and the area ratio shown by the
    // demo would be 100%.
    let full = mekiki.primary_screen()?;
    let scope_window = windows
        .iter()
        .find(|w| {
            w.bounds.width >= 400
                && w.bounds.height >= 300
                && area(w.bounds) < area(full.rect) * 9 / 10
        })
        .ok_or("no window large enough to use as a scope")?;
    let scope = mekiki.region(scope_window.bounds);

    println!("=== window scope ===");
    println!(
        "  target: {} {}",
        truncate(&scope_window.title, 50),
        scope.rect
    );
    let ratio = area(scope.rect) as f64 / area(full.rect) as f64;
    println!(
        "  {:.1}% of the whole screen by area (so the search is faster and false positives fewer)",
        ratio * 100.0
    );
    println!();

    // --- 2. lazy Target resolution ---
    println!("=== Target resolution (including the automatic wait) ===");
    let frame = mekiki.capture_region(scope)?;
    let (px, py, size) = pick_textured_patch(&frame, 80)?;
    let patch = frame
        .crop(Rect::new(
            frame.origin.0 + px as i32,
            frame.origin.1 + py as i32,
            size,
            size,
        ))
        .ok_or("cannot crop the patch")?;

    let pattern = mekiki
        .pattern_from_frame(&patch, "window_patch")
        .similar(0.9);
    let target = mekiki.target(scope, &pattern);

    let t0 = Instant::now();
    let found = mekiki.on(&target).resolve()?;
    let elapsed = t0.elapsed();

    let expected = (frame.origin.0 + px as i32, frame.origin.1 + py as i32);
    println!(
        "  {} score={:.4} / {:.0} ms",
        found.rect,
        found.score,
        elapsed.as_secs_f64() * 1000.0
    );
    println!("  expected position {expected:?} ... {}", {
        if (found.rect.x, found.rect.y) == expected {
            "match"
        } else {
            "mismatch (the screen may be moving)"
        }
    });
    println!("  note: this time includes one capture for the stability check");

    // force() skips the stability check. Measure the difference.
    let forced = target.clone().force();
    let t0 = Instant::now();
    mekiki.on(&forced).resolve()?;
    println!(
        "  with force(): {:.0} ms (which shows the cost of the stability check)",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    println!();

    // --- 3. highlight ---
    println!("=== highlight (outline shown for 1.2s) ===");
    match mekiki.on(&target).highlight(Duration::from_millis(1200)) {
        Ok(m) => println!("  drew an outline at {}", m.rect),
        Err(e) => println!("  [warning] cannot display: {e}"),
    }
    println!();

    // --- 4. expect().to_vanish() ---
    println!("=== expect().to_vanish() ===");
    // A pattern that is not on screen has "already vanished", so this succeeds
    // immediately.
    let absent = synthetic_pattern(&mekiki, 48, "absent_pattern");
    let absent_target = mekiki
        .target(scope, &absent)
        .timeout(Duration::from_secs(1));
    let t0 = Instant::now();
    match mekiki.expect(&absent_target).to_vanish(None) {
        Ok(()) => println!(
            "  an absent pattern is judged already vanished ({:.0} ms)",
            t0.elapsed().as_secs_f64() * 1000.0
        ),
        Err(e) => println!("  [warning] {e}"),
    }

    // A pattern that is on screen never vanishes, so this times out.
    let t0 = Instant::now();
    match mekiki
        .expect(&target)
        .to_vanish(Some(Duration::from_millis(600)))
    {
        Ok(()) => println!("  [warning] it was reported as vanished"),
        Err(e) => println!(
            "  a present pattern fails as expected ({:.0} ms):\n    {}",
            t0.elapsed().as_secs_f64() * 1000.0,
            first_line(&e.to_string())
        ),
    }
    println!();

    // --- 5. failure artifacts ---
    println!("=== failure artifacts ===");
    let missing = mekiki
        .target(scope, &absent)
        .similar(0.95)
        .timeout(Duration::from_millis(300));

    match mekiki.on(&missing).resolve() {
        Ok(m) => println!("  [warning] it was found after all: {}", m.rect),
        Err(e) => {
            println!("  failed as expected:");
            for line in e.to_string().lines() {
                println!("    {line}");
            }
        }
    }

    println!();
    println!("done. The mouse was never moved.");
    Ok(())
}

fn area(r: Rect) -> u64 {
    u64::from(r.width) * u64::from(r.height)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// Build a pattern that should not exist anywhere on screen.
fn synthetic_pattern(mekiki: &Mekiki, size: u32, name: &str) -> mekiki_core::Pattern {
    let mut state = 0x00C0_FFEEu32;
    let luma: Vec<u8> = (0..size * size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state % 256) as u8
        })
        .collect();
    mekiki.pattern_from_luma8(&luma, size, size, name)
}

/// Pick a textured position. A uniform area cannot become a pattern.
fn pick_textured_patch(
    frame: &mekiki_core::Frame,
    size: u32,
) -> Result<(u32, u32, u32), Box<dyn std::error::Error>> {
    if frame.width < size || frame.height < size {
        return Err(format!("the area is smaller than {size}px").into());
    }
    let luma = frame.to_luma_f32();
    let w = frame.width as usize;

    let mut best = (f64::NEG_INFINITY, 0u32, 0u32);
    for gy in 1..8u32 {
        for gx in 1..8u32 {
            let x = (frame.width - size) * gx / 8;
            let y = (frame.height - size) * gy / 8;
            let (mut sum, mut sq) = (0.0f64, 0.0f64);
            for row in 0..size as usize {
                let start = (y as usize + row) * w + x as usize;
                for v in &luma[start..start + size as usize] {
                    let v = f64::from(*v);
                    sum += v;
                    sq += v * v;
                }
            }
            let n = f64::from(size) * f64::from(size);
            let var = (sq - sum * sum / n) / n;
            if var > best.0 {
                best = (var, x, y);
            }
        }
    }

    if best.0.sqrt() < 1e-3 {
        return Err("the whole area is uniform, so there is nowhere to take a pattern from".into());
    }
    Ok((best.1, best.2, size))
}

// A reference to avoid an unused warning. The colour-taking highlight is public too.
#[allow(dead_code)]
fn _color_reference() -> HighlightColor {
    HighlightColor::GREEN
}

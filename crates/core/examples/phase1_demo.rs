//! Verifies the Phase 1 completion criteria on real hardware.
//!
//! The criterion (Phase 1 of the development plan): "screenshot, find, click
//! works end to end through the Rust API alone".
//!
//! What it does:
//!
//! 1. Captures the primary display and saves it as a PNG
//! 2. Crops part of the capture into a pattern
//! 3. **Captures again** and searches for that pattern. Matching coordinates
//!    mean capture, matching and the coordinate systems all line up
//! 4. Moves the mouse to the match, reads the cursor position back and compares.
//!    That also confirms input injection shares the capture's coordinate system
//! 5. Compares the time taken by the pyramid search and a full-size brute force
//!
//! ```text
//! cargo run --release -p mekiki-core --example phase1_demo
//! cargo run --release -p mekiki-core --example phase1_demo -- --click
//! ```
//!
//! It does not click by default. Touching the real screen means pressing who
//! knows what, so it stops at moving the mouse and returns it to its original
//! position at the end. It only clicks when `--click` is passed.

use std::time::{Duration, Instant};

use mekiki_core::search::{self, Matcher, SearchParams};
use mekiki_core::{Mekiki, Pattern};
use mekiki_matching::Image;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let do_click = std::env::args().any(|a| a == "--click");

    if let Err(e) = run(do_click) {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run(do_click: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut mekiki = Mekiki::new()?;

    println!("=== displays ===");
    for d in mekiki.displays() {
        println!(
            "  [{}] {} {} {}",
            d.index,
            d.name,
            d.bounds,
            if d.is_primary { "(primary)" } else { "" }
        );
    }
    println!("matching backend: {}", mekiki.matcher_backend());
    println!();

    // --- 1. capture ---
    let screen = mekiki.primary_screen()?;
    let t0 = Instant::now();
    let frame = mekiki.capture_region(screen)?;
    let capture_ms = t0.elapsed().as_secs_f64() * 1000.0;

    println!("=== capture ===");
    println!(
        "  {}x{} origin={:?} / {:.1} ms",
        frame.width, frame.height, frame.origin, capture_ms
    );

    let out = std::path::Path::new("bench-results").join("phase1_capture.png");
    std::fs::create_dir_all("bench-results")?;
    save_png(&frame, &out)?;
    println!("  saved to {}", out.display());
    println!("  backend: {}", mekiki.capture_diagnostics());
    println!();

    // --- 2. turn part of the capture into a pattern ---
    //
    // The top-left of the screen is often a uniform stretch of wallpaper, so a
    // textured place is chosen instead. A uniform area drives the denominator to
    // 0 and cannot serve as a pattern at all.
    let patch_size = 96u32;
    let (px, py) = pick_textured_patch(&frame, patch_size)?;
    let patch = frame
        .crop(mekiki_capture::Rect::new(
            frame.origin.0 + px as i32,
            frame.origin.1 + py as i32,
            patch_size,
            patch_size,
        ))
        .ok_or("cannot crop the patch")?;

    let pattern = mekiki
        .pattern_from_frame(&patch, "screen_patch")
        .similar(0.9);
    println!("=== pattern ===");
    println!(
        "  cropped {patch_size}x{patch_size} from ({px}, {py}) / {} pyramid levels",
        pattern.max_level()
    );
    println!();

    // --- 3. search for it again ---
    println!("=== find (re-capture and search) ===");
    let t0 = Instant::now();
    let found = mekiki.find(screen, &pattern)?;
    let find_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let expected = (frame.origin.0 + px as i32, frame.origin.1 + py as i32);
    println!(
        "  found: {} score={:.4} / {:.1} ms (including the capture)",
        found.rect, found.score, find_ms
    );
    println!("  expected position: {expected:?}");

    if (found.rect.x, found.rect.y) != expected {
        // Movement on screen (a clock, the cursor, an animation) can make
        // another place score best. That must be distinguished from a broken
        // coordinate system, so this stays a warning.
        println!(
            "  [warning] the position is off by ({}, {}). \
             The screen may be moving",
            found.rect.x - expected.0,
            found.rect.y - expected.1
        );
    } else {
        println!("  coordinates match. Capture, matching and the coordinate systems line up");
    }
    println!();

    // --- 4. input injection ---
    println!("=== input (mouse movement) ===");
    let original = mekiki.mouse_position()?;
    let target = found.target();
    println!("  current cursor: {original:?}");
    println!("  destination: {target:?}");

    // Try several times and take the best: moving the physical mouse during the
    // run shows up directly as error, which happened on the first measurement.
    // Starting from i32::MAX and comparing sums overflows — in release it wraps
    // and the comparison is always false, which also happened — so this uses an
    // Option instead.
    let mut best_err: Option<(i32, i32)> = None;
    let mut after = (0, 0);
    for _ in 0..3 {
        mekiki.hover(target)?;
        std::thread::sleep(Duration::from_millis(40));
        let got = mekiki.mouse_position()?;
        let d = ((got.0 - target.0).abs(), (got.1 - target.1).abs());
        if best_err.is_none_or(|b| d.0 + d.1 < b.0 + b.1) {
            best_err = Some(d);
            after = got;
        }
    }
    let best = best_err.unwrap_or((i32::MAX, i32::MAX));
    println!("  cursor after the move: {after:?} (best of 3)");

    if best.0 <= 1 && best.1 <= 1 {
        // Absolute coordinates round through 0..65535, so 1px of error is expected.
        println!(
            "  coordinates match (error {}, {} px). Input and capture share a coordinate system",
            best.0, best.1
        );
    } else {
        println!(
            "  [warning] off by {}, {} px. Suspect the DPI setting or how virtual desktop coordinates are handled",
            best.0, best.1
        );
        println!("  (moving the physical mouse during the run also lands here)");
    }

    if do_click {
        println!("  --click was given, so clicking");
        mekiki.click(target)?;
    } else {
        println!("  not clicking (pass --click to)");
    }

    mekiki.hover(original)?;
    println!("  returned the cursor to its original position");
    println!();

    // --- 5. the effect of the pyramid search ---
    println!("=== pyramid search vs full-size brute force ===");
    compare_search_strategies(&frame, &pattern);

    Ok(())
}

/// Pick a high-variance position from an already-captured frame.
///
/// A uniform area drives the ZMD denominator to 0, which takes the early return
/// that fills the map without dispatching, leaving nothing to verify.
fn pick_textured_patch(frame: &mekiki_capture::Frame, size: u32) -> Result<(u32, u32), String> {
    if frame.width < size || frame.height < size {
        return Err(format!("the screen is smaller than {size}px"));
    }
    let luma = frame.to_luma_f32();
    let w = frame.width as usize;

    let mut best = (f64::NEG_INFINITY, 0u32, 0u32);
    for gy in 1..8u32 {
        for gx in 1..8u32 {
            let x = (frame.width - size) * gx / 8;
            let y = (frame.height - size) * gy / 8;
            let mut sum = 0.0f64;
            let mut sq = 0.0f64;
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
        return Err(
            "the whole screen is uniform, so there is nowhere to take a pattern from".into(),
        );
    }
    Ok((best.1, best.2))
}

fn compare_search_strategies(frame: &mekiki_capture::Frame, pattern: &Pattern) {
    let haystack = Image::new(frame.to_luma_f32(), frame.width, frame.height);
    let mut matcher = Matcher::best_available();

    let levels = search::build_haystack_pyramid(&haystack, pattern.max_level());

    let t0 = Instant::now();
    let pyr = search::find(
        &mut matcher,
        &levels,
        pattern,
        &SearchParams::default(),
        false,
    );
    let pyr_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // A 0-level pyramid is a full-size brute force.
    let flat = vec![haystack.clone().into_owned()];
    let t0 = Instant::now();
    let full = search::find(
        &mut matcher,
        &flat,
        pattern,
        &SearchParams::default(),
        false,
    );
    let full_ms = t0.elapsed().as_secs_f64() * 1000.0;

    println!(
        "  pyramid ({} levels): {:>8.1} ms  {:?}",
        pattern.max_level(),
        pyr_ms,
        pyr.first().map(|h| (h.x, h.y, h.score))
    );
    println!(
        "  full-size brute force: {:>8.1} ms  {:?}",
        full_ms,
        full.first().map(|h| (h.x, h.y, h.score))
    );
    if pyr_ms > 0.0 {
        println!("  => {:.1}x faster", full_ms / pyr_ms);
    }

    match (pyr.first(), full.first()) {
        (Some(a), Some(b)) if (a.x, a.y) == (b.x, b.y) => {
            println!("  the two agree");
        }
        (Some(a), Some(b)) => {
            println!(
                "  [warning] the results differ: pyramid {:?} / full size {:?}",
                (a.x, a.y),
                (b.x, b.y)
            );
        }
        _ => println!("  [warning] one of them found nothing"),
    }
}

fn save_png(
    frame: &mekiki_capture::Frame,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // BGRA -> RGBA
    let mut rgba = Vec::with_capacity(frame.bgra.len());
    for px in frame.bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    let buf = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or("the image buffer size does not match")?;
    buf.save(path)?;
    Ok(())
}

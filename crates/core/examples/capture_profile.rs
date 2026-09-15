//! Measures the breakdown of a single `find()`.
//!
//! Material for deciding whether Phase 4-3 (zero copy) of the development plan
//! is worth starting.
//!
//! ```text
//! cargo run --release -p mekiki-core --example capture_profile
//! ```
//!
//! Zero copy could eliminate three things.
//!
//! - **Readback**: bringing a GPU texture into CPU memory (part of the capture)
//! - **Luma conversion**: BGRA to f32 greyscale, across every pixel on the CPU
//! - **Upload**: sending that f32 back to the GPU
//!
//! What it cannot eliminate is the matching itself and the readback of the
//! result. Knowing the ratio between them shows the ceiling for zero copy.

use std::time::Instant;

use mekiki_matching::{Image, MatchMethod, TemplateMatcher};

const ITERS: usize = 10;

fn main() {
    if let Err(e) = run() {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut capture = mekiki_capture::open()?;
    let display = capture
        .displays()
        .iter()
        .find(|d| d.is_primary)
        .or_else(|| capture.displays().first())
        .ok_or("no display")?
        .index;

    let probe = capture.capture(display)?;
    println!("screen {}x{}", probe.width, probe.height);
    println!("backend {}", capture.diagnostics());
    println!();

    // --- capture (AcquireNextFrame + CopyResource + Map + memcpy) ---
    let mut capture_ms = Vec::new();
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let _ = capture.capture(display)?;
        capture_ms.push(ms(t0));
    }

    let frame = capture.capture(display)?;

    // --- luma conversion (BGRA -> f32) ---
    let mut luma_ms = Vec::new();
    let mut luma = Vec::new();
    for _ in 0..ITERS {
        let t0 = Instant::now();
        luma = frame.to_luma_f32();
        luma_ms.push(ms(t0));
    }

    let haystack = Image::new(luma, frame.width, frame.height);

    // --- matching (upload + compute + result readback) ---
    let mut matcher = TemplateMatcher::new()?;
    println!("GPU {}", matcher.adapter_info().name);
    println!();

    let mut rows = Vec::new();
    for tsize in [32u32, 96u32] {
        // Crop from a textured position. A fixed position can land on a uniform
        // area, which drives the ZMD denominator to 0 and takes the early
        // return, **producing a fast number without running matching at all**.
        let template = pick_textured(&haystack, tsize)?;

        // Warm up, so buffer allocation stays out of the measurement.
        for _ in 0..2 {
            let _ = matcher.match_template(&haystack, &template, MatchMethod::ZeroMeanDice);
        }

        let mut match_ms = Vec::new();
        for _ in 0..ITERS {
            let t0 = Instant::now();
            let _ = matcher
                .match_template(&haystack, &template, MatchMethod::ZeroMeanDice)
                .expect("matching failed");
            match_ms.push(ms(t0));
        }
        rows.push((tsize, median(&mut match_ms)));
    }

    let cap = median(&mut capture_ms);
    let lum = median(&mut luma_ms);

    println!("{:<28} {:>10}", "stage", "median[ms]");
    println!("{}", "-".repeat(42));
    println!("{:<28} {:>10.1}", "capture (incl. readback)", cap);
    println!("{:<28} {:>10.1}", "luma conversion BGRA->f32", lum);
    for (tsize, t) in &rows {
        println!(
            "{:<28} {:>10.1}",
            format!("matching tpl={tsize} (incl. transfer)"),
            t
        );
    }
    println!();

    for (tsize, t) in &rows {
        let total = cap + lum + t;
        // In principle zero copy removes readback, luma conversion and upload.
        // Readback and upload cannot be separated out here, so the luma
        // conversion alone is shown as the floor of what is certainly removable.
        println!(
            "tpl={tsize}: total {total:.1} ms (capture {:.0}% / luma {:.0}% / matching {:.0}%)",
            cap / total * 100.0,
            lum / total * 100.0,
            t / total * 100.0
        );
    }

    println!();
    println!(
        "for reference, the luma conversion is a CPU pass over {} pixels.",
        frame.width * frame.height
    );
    println!("Zero copy removes the luma conversion and the CPU-GPU transfers");
    println!("contained within \"capture\" and \"matching\".");

    Ok(())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Crop a template from the position with the greatest variance.
fn pick_textured(src: &Image<'_>, size: u32) -> Result<Image<'static>, Box<dyn std::error::Error>> {
    let mut best = (f64::NEG_INFINITY, 0u32, 0u32);
    for gy in 1..8u32 {
        for gx in 1..8u32 {
            let x = (src.width - size) * gx / 8;
            let y = (src.height - size) * gy / 8;
            let patch = crop(src, x, y, size, size);
            let n = f64::from(size) * f64::from(size);
            let sum: f64 = patch.data.iter().map(|&v| f64::from(v)).sum();
            let sq: f64 = patch
                .data
                .iter()
                .map(|&v| f64::from(v) * f64::from(v))
                .sum();
            let var = (sq - sum * sum / n) / n;
            if var > best.0 {
                best = (var, x, y);
            }
        }
    }
    if best.0.sqrt() < 1e-3 {
        return Err("the screen is uniform, so no template can be taken".into());
    }
    Ok(crop(src, best.1, best.2, size, size))
}

fn crop(src: &Image<'_>, x: u32, y: u32, w: u32, h: u32) -> Image<'static> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for j in 0..h {
        let row = ((y + j) as usize) * (src.width as usize) + x as usize;
        out.extend_from_slice(&src.data[row..row + w as usize]);
    }
    Image::new(out, w, h)
}

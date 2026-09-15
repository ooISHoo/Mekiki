//! Catches score-1.0 false positives on a real screen and saves them in a
//! reproducible form.
//!
//! **Note (2026-08-15)**: this tool was written to investigate false positives
//! in the NCC era. The score is now ZMD
//! (`docs/architecture/matching.md`), so the "NCC numerator and denominator"
//! diagnostics below belong to that investigation record. The window-variance
//! diagnostic still works unchanged under ZMD as a way to separate "has
//! information" from "uniform", so it is kept.
//!
//! ```text
//! # 1) crop a template out of a running application
//! cargo run --release -p mekiki-core --example false_positive_probe -- grab 1100 560 260 34 tpl.png
//!
//! # 2) repeat until a false positive appears, saving that screen as a PNG
//! cargo run --release -p mekiki-core --example false_positive_probe -- watch tpl.png 200
//!
//! # 3) reproduce it from the saved screen as often as you like
//! cargo run --release -p mekiki-core --example false_positive_probe -- replay bogus-1.png tpl.png
//! ```
//!
//! # Why saving matters
//!
//! The reported symptom is that **the place changes every time and it sometimes
//! does not reproduce**. If the cause lies in the screen content, keeping the
//! screen from the moment it happened makes everything from there on
//! deterministic. If the same screen instead gives different results each time,
//! suspect non-determinism in the matcher. `replay` covers that too, running the
//! same input five times and comparing.
//!
//! # What it reports
//!
//! For each high-scoring position it recomputes the NCC numerator and
//! denominator in f64 and prints them.
//!
//! ```text
//! den = sqrt(max(sum(I^2) - sum(I)^2/N, 0)) * template_norm
//! ```
//!
//! The shader returned `num/den` when `|num| < den`, and **exactly ±1.0** when
//! `|num| < den*1.125` — inherited from OpenCV. In a uniform window both num and
//! den become noise after cancellation, so a ratio that happens to land in that
//! band yields 1.0. A `den` that is extremely small, or a `|num|/den` between
//! 1.0 and 1.125, is the cause.

use std::path::Path;

#[allow(unused_imports)]
use mekiki_capture::ScreenCapture;
use mekiki_matching::{Image, MatchMethod, TemplateMatcher};

/// At or above this counts as a hit.
const HIT: f32 = 0.9999;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Err(e) = run() {
        eprintln!("failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("grab") => grab(&args[1..]),
        Some("watch") => watch(&args[1..]),
        Some("replay") => replay(&args[1..]),
        Some("sweep") => sweep(&args[1..]),
        Some("pipeline") => pipeline(&args[1..]),
        _ => {
            eprintln!("usage:");
            eprintln!("  grab <x> <y> <w> <h> <out.png>            crop from the screen and save");
            eprintln!(
                "  watch <template.png> [rounds] [expected]   loop until a false positive and save the screen"
            );
            eprintln!("  replay <screen.png> <template.png>        re-examine a saved screen");
            Err("specify a command".into())
        }
    }
}

fn grab(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 5 {
        return Err("grab <x> <y> <w> <h> <out.png>".into());
    }
    let rect = mekiki_capture::Rect::new(
        args[0].parse()?,
        args[1].parse()?,
        args[2].parse()?,
        args[3].parse()?,
    );
    let mut capture = mekiki_capture::open()?;
    let frame = capture.capture_rect(rect)?;
    save_frame(&frame, &args[4])?;
    println!("saved to {} ({}x{})", args[4], frame.width, frame.height);
    Ok(())
}

/// Keep capturing until a false positive appears, then save that screen.
fn watch(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let tpl_path = args.first().ok_or("specify a template image")?;
    let rounds: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
    // How many should be on screen. One by default.
    let expected: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);

    let template = load_luma(tpl_path)?;
    report_template(tpl_path, &template);

    let mut capture = mekiki_capture::open()?;
    let mut matcher = TemplateMatcher::new()?;
    println!("GPU {}\n", matcher.adapter_info().name);

    let mut saved = 0usize;
    for round in 1..=rounds {
        let frame = capture.capture(0)?;
        let haystack = Image::new(frame.to_luma_f32(), frame.width, frame.height);
        let hits = find_hits(&mut matcher, &haystack, &template)?;

        if hits.len() <= expected {
            if round % 20 == 0 {
                println!(
                    "nothing unusual through round {round} ({} hits)",
                    hits.len()
                );
            }
            continue;
        }

        saved += 1;
        let path = format!("bogus-{saved}.png");
        save_frame(&frame, &path)?;
        println!(
            "--- round {round}: {} hits (expected {expected}) ---",
            hits.len()
        );
        println!("saved the screen to {path}. Re-examine it with replay.");
        report_hits(&haystack, &template, &hits);
        println!();

        if saved >= 3 {
            println!("three examples collected, stopping.");
            return Ok(());
        }
    }

    if saved == 0 {
        println!("\nno false positive in {rounds} rounds. Increase the count, or");
        println!("try a screen more prone to them (one with wide bands of a single colour).");
    }
    Ok(())
}

/// Re-examine a saved screen. **Runs five times on the same input to check for
/// non-determinism as well.**
fn replay(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("replay <screen.png> <template.png>".into());
    }
    let haystack = load_luma(&args[0])?;
    let template = load_luma(&args[1])?;
    println!(
        "screen {} ({}x{})",
        args[0], haystack.width, haystack.height
    );
    report_template(&args[1], &template);

    let mut matcher = TemplateMatcher::new()?;
    println!("GPU {}\n", matcher.adapter_info().name);

    let mut first: Option<Vec<(u32, u32)>> = None;
    for round in 1..=5 {
        let hits = find_hits(&mut matcher, &haystack, &template)?;
        let positions: Vec<(u32, u32)> = hits.iter().map(|h| (h.0, h.1)).collect();

        match &first {
            None => {
                println!("{} hits", hits.len());
                report_hits(&haystack, &template, &hits);
                first = Some(positions);
            }
            Some(f) if *f != positions => {
                println!(
                    "**the result changed on round {round}.** Non-deterministic on identical input, so the problem is in the matcher."
                );
                println!("  round 1: {f:?}");
                println!("  round {round}: {positions:?}");
                return Ok(());
            }
            _ => {}
        }
    }

    println!("\nall five rounds agree: same input, same result, so there is no non-determinism.");
    println!("If a false positive remains, the cause is the screen content and the nature of NCC.");
    Ok(())
}

/// Investigate along **the same route as a real match**: pyramid search plus
/// refinement.
///
/// A raw `match_template` did not reproduce it, so this is the suspect. The
/// search crops a small window per candidate and re-matches, and the input mean
/// is subtracted within that window. In a nearly uniform window the DC component
/// disappears and both numerator and denominator become noise.
///
/// When the image being searched for is not on screen, the coarse threshold
/// drops to its floor and candidates flood in from uniform areas. That should be
/// the condition under which this surfaces.
fn pipeline(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let tpl_path = args.first().ok_or("specify a template image")?;
    let rounds: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);

    let mut mekiki = mekiki_core::Mekiki::new()?;
    let screen = mekiki.primary_screen()?;
    let pattern = mekiki.pattern_from_file(tpl_path)?;
    println!(
        "template {tpl_path} ({}x{}) threshold {:.2}",
        pattern.width(),
        pattern.height(),
        pattern.similarity()
    );
    println!("search area {}\n", screen.rect);

    for round in 1..=rounds {
        let target = mekiki.target(screen, &pattern);
        let found = match mekiki.on(&target).resolve_all() {
            Ok(v) => v,
            Err(mekiki_core::Error::NotFound(_)) => {
                println!("round {round}: not found (correct)");
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        println!("round {round}: {} hits", found.len());

        // Measure the window variance at each hit to separate real ones from noise.
        let frame = mekiki.capture_region(screen)?;
        let hay = Image::new(frame.to_luma_f32(), frame.width, frame.height);

        for m in found.iter().take(8) {
            let x = (m.rect.x - frame.origin.0).max(0) as u32;
            let y = (m.rect.y - frame.origin.1).max(0) as u32;
            if x + m.rect.width > hay.width || y + m.rect.height > hay.height {
                continue;
            }
            let var = window_variance(&hay, x, y, m.rect.width, m.rect.height);
            let verdict = if var < 1e-6 {
                "uniform window = false positive"
            } else {
                "has information"
            };
            println!(
                "  ({:>5},{:>5}) score {:.4}  window variance {var:.3e}  {verdict}",
                m.rect.x, m.rect.y, m.score
            );
        }
    }
    Ok(())
}

/// Sweep the brightness of a uniform background and see what appears.
///
/// The reported conditions were "**the image being searched for is not on
/// screen**" plus "there is a wide, uniformly white area". In a uniform window
/// both the NCC numerator and denominator become noise after cancellation, so
/// the ratio should behave differently depending on the background value. This
/// sweeps to find which brightness misbehaves.
fn sweep(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let tpl_path = args.first().ok_or("specify a template image")?;
    let template = load_luma(tpl_path)?;
    report_template(tpl_path, &template);

    let mut matcher = TemplateMatcher::new()?;
    println!("GPU {}\n", matcher.adapter_info().name);

    // Build a nearly uniform image that **does not contain** the template.
    let w = template.width * 3;
    let h = template.height * 6;
    let level: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1.0);

    // With perfect uniformity (zero noise) both numerator and denominator reach
    // 0 and a plain 0.0 comes back. A real screen fluctuates very slightly, so
    // this varies that amount. One 8-bit step is 1/255, about 0.0039.
    println!("background brightness {level:.2}, sweeping the amount of noise");
    println!(
        "{:<16} {:>10} {:>10}",
        "noise (LSB ratio)", "max score", "count of 1.0"
    );
    println!("{}", "-".repeat(40));

    let mut worst = 0.0f32;
    for step in 0..=12 {
        // Step by orders of magnitude: 0, 1e-6, 1e-5 and so on, to look closely
        // between 0 and 1 LSB.
        let amp = if step == 0 {
            0.0
        } else {
            (1.0 / 255.0) * 10f32.powi(-(12 - step) / 2)
        };

        // A deterministic pseudo-random sequence; varying per run would make the
        // comparison meaningless.
        let mut seed = 0x9e3779b9u32;
        let data: Vec<f32> = (0..(w * h))
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                level + r * amp
            })
            .collect();
        let noisy = Image::new(data, w, h);

        let scores = matcher.match_template(&noisy, &template, MatchMethod::ZeroMeanDice)?;
        let max = scores.data.iter().copied().fold(f32::MIN, f32::max);
        let ones = scores.data.iter().filter(|&&s| s >= HIT).count();
        worst = worst.max(max);

        let mark = if ones > 0 { "  <- false positive" } else { "" };
        let label = if amp == 0.0 {
            "0 (perfectly uniform)".to_string()
        } else {
            format!("{:.2e}", amp * 255.0)
        };
        println!("{label:<16} {max:>10.4} {ones:>10}{mark}");
    }

    println!();
    if worst >= HIT {
        println!(
            "**1.0 appeared on a nearly uniform image.** A guard is needed for windows whose denominator is noise."
        );
    } else {
        println!("no 1.0 under these conditions. Maximum {worst:.4}");
    }
    Ok(())
}

/// List the high-scoring positions with overlaps removed.
fn find_hits(
    matcher: &mut TemplateMatcher,
    haystack: &Image<'_>,
    template: &Image<'_>,
) -> Result<Vec<(u32, u32, f32)>, Box<dyn std::error::Error>> {
    let scores = matcher.match_template(haystack, template, MatchMethod::ZeroMeanDice)?;

    let mut hits: Vec<(u32, u32, f32)> = scores
        .data
        .iter()
        .enumerate()
        .filter(|(_, s)| **s >= HIT)
        .map(|(i, &s)| ((i as u32) % scores.width, (i as u32) / scores.width, s))
        .collect();

    // Adjacent hits are the same thing, so fold them into one.
    hits.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut kept: Vec<(u32, u32, f32)> = Vec::new();
    for h in hits {
        let overlaps = kept
            .iter()
            .any(|k| k.0.abs_diff(h.0) < template.width && k.1.abs_diff(h.1) < template.height);
        if !overlaps {
            kept.push(h);
        }
    }
    Ok(kept)
}

fn report_template(path: &str, template: &Image<'_>) {
    let (mean, var) = mean_and_variance(&template.data);
    let norm = (var * template.data.len() as f64).sqrt();
    println!(
        "template {path} ({}x{}) mean {mean:.4} variance {var:.3e} norm {norm:.4}",
        template.width, template.height
    );
    if var < 1e-5 {
        println!(
            "**the template is nearly uniform.** It carries little information and hits easily anywhere."
        );
    }
}

fn report_hits(haystack: &Image<'_>, template: &Image<'_>, hits: &[(u32, u32, f32)]) {
    println!(
        "  {:<14} {:>11} {:>11} {:>9} {:>11}",
        "position", "window var", "numerator", "num/den", "verdict"
    );
    for &(x, y, _) in hits {
        let var = window_variance(haystack, x, y, template.width, template.height);
        let (num, den) = num_and_den(haystack, template, x, y);
        let ratio = if den > 0.0 {
            num.abs() / den
        } else {
            f64::INFINITY
        };

        // **The ratio alone cannot separate them.** A genuine match also gives
        // exactly 1.0 (the shader returned ±1.0 when |num| was at least den and
        // below den*1.125).
        //
        // What matters is whether the window carries information. One 8-bit step
        // is 1/255, about 0.0039, so a window whose variance is below half that
        // squared (about 4e-6) is uniform within quantization. Such a window
        // cannot correlate with any template.
        const FLAT_VAR: f64 = 1e-6;
        let verdict = if var < FLAT_VAR {
            "uniform window = false positive"
        } else {
            "has information = genuine"
        };
        println!("  ({x:>5},{y:>5}) {var:>11.3e} {num:>11.3e} {ratio:>9.4} {verdict:>11}");
    }
}

fn save_frame(frame: &mekiki_core::Frame, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let rgba: Vec<u8> = frame
        .bgra
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect();
    image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or("the pixel count does not match")?
        .save(path)?;
    Ok(())
}

/// Turn a PNG into a luma `Image`, using the same coefficients as
/// `Frame::to_luma_f32`.
fn load_luma(path: impl AsRef<Path>) -> Result<Image<'static>, Box<dyn std::error::Error>> {
    let img = image::open(path)?.to_rgba8();
    let data: Vec<f32> = img
        .pixels()
        .map(|p| {
            (0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])) / 255.0
        })
        .collect();
    Ok(Image::new(data, img.width(), img.height()))
}

fn mean_and_variance(data: &[f32]) -> (f64, f64) {
    let n = data.len() as f64;
    let mean = data.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
    let var = data
        .iter()
        .map(|&v| {
            let d = f64::from(v) - mean;
            d * d
        })
        .sum::<f64>()
        / n;
    (mean, var)
}

/// Measure a window's variance correctly in f64.
///
/// The shader computes `sum(I^2) - sum(I)^2/N` in f32, which cancels down to
/// noise for a uniform window. This subtracts the mean before summing squares,
/// so it is unaffected.
fn window_variance(img: &Image<'_>, x: u32, y: u32, w: u32, h: u32) -> f64 {
    let mut values = Vec::with_capacity((w * h) as usize);
    for j in 0..h {
        let row = ((y + j) as usize) * (img.width as usize) + x as usize;
        values.extend_from_slice(&img.data[row..row + w as usize]);
    }
    mean_and_variance(&values).1
}

/// Compute the NCC numerator and denominator at that position in f64.
fn num_and_den(img: &Image<'_>, template: &Image<'_>, x: u32, y: u32) -> (f64, f64) {
    let area = f64::from(template.width) * f64::from(template.height);
    let t_mean = template.data.iter().map(|&v| f64::from(v)).sum::<f64>() / area;
    let t_norm = template
        .data
        .iter()
        .map(|&v| {
            let c = f64::from(v) - t_mean;
            c * c
        })
        .sum::<f64>()
        .sqrt();

    let (mut sum_i, mut sum_i2, mut sum_it) = (0.0f64, 0.0f64, 0.0f64);
    for j in 0..template.height {
        let row = ((y + j) as usize) * (img.width as usize) + x as usize;
        let t_row = (j * template.width) as usize;
        for i in 0..template.width {
            let v = f64::from(img.data[row + i as usize]);
            let t = f64::from(template.data[t_row + i as usize]) - t_mean;
            sum_i += v;
            sum_i2 += v * v;
            sum_it += v * t;
        }
    }
    let den = (sum_i2 - sum_i * sum_i / area).max(0.0).sqrt() * t_norm;
    (sum_it, den)
}

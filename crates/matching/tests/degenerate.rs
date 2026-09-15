//! Pins down how ZMD behaves over uniform regions.
//!
//! # What this protects
//!
//! Back in the NCC era, a real application (a game's menu) produced
//! **false positives with a score of 1.0 over areas containing nothing**.
//! The location changed every time and it did not always reproduce.
//! The cause was that the NCC denominator `sqrt(dev2) * tn` goes to 0 on a
//! uniform window, and the resulting noise ratio was turned into ±1.0 by the
//! clamp inherited from OpenCV. It was first patched with a uniform-window
//! guard (`MIN_WINDOW_STDDEV`), then removed at the root on 2026-08-15 by
//! switching to ZMD (see docs/architecture/matching.md).
//!
//! The ZMD denominator is held up from below by `dev2 + tn2 >= tn2 > 0`:
//!
//! ```text
//! s = 2·Σ(I'·T') / (Σ I'² + Σ T'²)
//! ```
//!
//! On a uniform window both the numerator and dev2 are ~0, and s falls
//! continuously to ~0 **without any guard**. The tests here pin both halves:
//! that uniform or near-uniform input produces no high score, and that a real
//! match still survives.

use mekiki_matching::{Image, MatchMethod, TemplateMatcher};

/// The background shade of the menu bar. Used both for most of the template and
/// for the uniform part of the background.
///
/// **The point is that the background and the template's base are the same
/// shade.** The real application also produced false positives where a band of
/// one colour covered a large part of the screen. Under NCC both numerator and
/// denominator for such a window became post-cancellation noise, and their
/// ratio ran wild.
const BAR: f32 = 0.15;

/// A uniform background with a single menu-item-like patch placed on it.
fn scene(width: u32, height: u32, patch_at: (u32, u32), patch: &Image<'_>) -> Image<'static> {
    let mut data = vec![BAR; (width * height) as usize];
    for j in 0..patch.height {
        for i in 0..patch.width {
            let x = patch_at.0 + i;
            let y = patch_at.1 + j;
            data[(y * width + x) as usize] = patch.data[(j * patch.width + i) as usize];
        }
    }
    Image::new(data, width, height)
}

/// A patch standing in for a menu item.
///
/// **Mostly the base shade, with only a few bright pixels for the text.**
/// This matches the real thing (small white text on a dark band). Carrying
/// little information makes the template energy tn2 small, which under NCC
/// shrank the denominator twice over.
fn patch(w: u32, h: u32) -> Image<'static> {
    let mut data = vec![BAR; (w * h) as usize];
    // Scatter the "text" pixels: just under 10% of the area.
    for y in 2..h.saturating_sub(2) {
        for x in 2..w.saturating_sub(2) {
            if (x * 5 + y * 11) % 23 == 0 {
                data[(y * w + x) as usize] = 0.92;
            }
        }
    }
    Image::new(data, w, h)
}

/// **A uniform background must never score 1.0.**
///
/// If this fails, the basic ZMD property that a uniform window falls to 0 has
/// been broken.
#[test]
fn uniform_background_never_scores_one() {
    let tpl = patch(240, 32);
    let at = (200u32, 150u32);
    let haystack = scene(640, 400, at, &tpl);

    let mut matcher = match TemplateMatcher::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping: no usable GPU: {e}");
            return;
        }
    };

    let scores = matcher
        .match_template(&haystack, &tpl, MatchMethod::ZeroMeanDice)
        .expect("matching failed");

    let mut bogus = Vec::new();
    for (idx, &s) in scores.data.iter().enumerate() {
        if s < 0.9 {
            continue;
        }
        let x = (idx as u32) % scores.width;
        let y = (idx as u32) / scores.width;
        // A window overlapping the real patch at all is expected to score high.
        // The "text" is a repeating pattern, so even offset positions correlate
        // well. What we want to see here is **windows with no overlap at all**.
        let overlaps = x.abs_diff(at.0) < tpl.width && y.abs_diff(at.1) < tpl.height;
        if !overlaps {
            bogus.push((x, y, s));
        }
    }

    assert!(
        bogus.is_empty(),
        "high scores over the uniform background (first 5): {:?}",
        &bogus[..bogus.len().min(5)]
    );

    // And the real match is still found. Suppressing the uniform case must not
    // take the genuine one with it.
    let hit = scores.data[(at.1 * scores.width + at.0) as usize];
    assert!(hit > 0.99, "the real match was lost: {hit}");
}

/// CPU and GPU must not diverge over a uniform background.
///
/// Under NCC the f32/f64 decision could land on either side of the guard
/// threshold; ZMD has no threshold at all, so every point should agree.
#[test]
fn cpu_and_gpu_agree_on_uniform_background() {
    let tpl = patch(160, 28);
    let at = (100u32, 80u32);
    let haystack = scene(480, 320, at, &tpl);

    let mut gpu = match TemplateMatcher::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping: no usable GPU: {e}");
            return;
        }
    };

    let g = gpu
        .match_template(&haystack, &tpl, MatchMethod::ZeroMeanDice)
        .expect("GPU matching failed");
    let c = mekiki_matching::cpu::match_template(&haystack, &tpl, MatchMethod::ZeroMeanDice)
        .expect("CPU matching failed");

    let mut worst = 0.0f32;
    let mut worst_at = (0u32, 0u32);
    for (idx, (&a, &b)) in g.data.iter().zip(c.data.iter()).enumerate() {
        let d = (a - b).abs();
        if d > worst {
            worst = d;
            worst_at = ((idx as u32) % g.width, (idx as u32) / g.width);
        }
    }
    assert!(
        worst < 0.01,
        "CPU and GPU disagree: max difference {worst} @ {worst_at:?}"
    );
}

/// **Reproduces the refinement window.** This is where the NCC false positives
/// came from.
///
/// The pyramid search crops a small `template + 10px margin` window per
/// candidate and re-matches inside it (`search::find` in `mekiki-core`).
/// The matcher subtracts the mean of the input, so a near-uniform window loses
/// its DC component and nothing is left. Under NCC this became 0/0.
///
/// With ZMD the result is s ≈ 2g/(1+g²), where g is the window's residual
/// contrast over the template's contrast. A window with only slight variation
/// has g ≪ 1, so s is tiny — continuously, deterministically and without any
/// guard.
#[test]
fn nearly_uniform_refine_window_scores_near_zero() {
    let tpl = patch(240, 32);

    let mut matcher = match TemplateMatcher::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping: no usable GPU: {e}");
            return;
        }
    };

    // An all-white window with a tiny amount of variation mixed in, tried at
    // several strengths. On a real screen the edge of a white region is never
    // perfectly uniform either.
    let margin = 10u32;
    let (w, h) = (tpl.width + margin, tpl.height + margin);

    let mut worst = 0.0f32;
    let mut worst_case = String::new();

    for &n_diff in &[1usize, 3, 8, 20, 60] {
        for &delta in &[1.0f32 / 255.0, 2.0 / 255.0, 8.0 / 255.0] {
            let mut data = vec![1.0f32; (w * h) as usize];
            // Scatter a few slightly darker pixels.
            for k in 0..n_diff {
                let i = (k * 7919) % data.len();
                data[i] = 1.0 - delta;
            }
            let window = Image::new(data, w, h);

            let scores = matcher
                .match_template(&window, &tpl, MatchMethod::ZeroMeanDice)
                .expect("matching failed");
            let max = scores.data.iter().copied().fold(f32::MIN, f32::max);
            if max > worst {
                worst = max;
                worst_case = format!("{n_diff} differing pixels / delta {delta:.4}");
            }
        }
    }

    // The bound in the NCC-plus-guard era was 0.5. ZMD decays continuously with
    // g ≪ 1, so we can demand an order of magnitude better.
    assert!(
        worst < 0.05,
        "a near-uniform window scored high: {worst} ({worst_case})"
    );
}

/// A completely uniform image must yield no match.
///
/// "Not there" is the correct answer, and `0.0` is what should come back.
#[test]
fn completely_flat_scene_finds_nothing() {
    let tpl = patch(120, 24);
    let haystack = Image::new(vec![0.37f32; 160 * 120], 160, 120);

    let mut matcher = match TemplateMatcher::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping: no usable GPU: {e}");
            return;
        }
    };

    let scores = matcher
        .match_template(&haystack, &tpl, MatchMethod::ZeroMeanDice)
        .expect("matching failed");

    let max = scores.data.iter().copied().fold(f32::MIN, f32::max);
    assert!(
        max < 0.01,
        "something was found in a uniform image: max {max}"
    );
}

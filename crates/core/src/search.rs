//! The coarse-search then refine loop.
//!
//! Assembled by the caller, outside the matching crate (as Phase 1-3 of the
//! plan directs). The matching crate only knows how to brute-force one needle
//! against one haystack.

use mekiki_matching::{
    Image, MatchMethod, NmsOptions, TemplateMatcher, cpu_fast::FastCpuMatcher, find_extremes,
    find_matches,
};

use crate::model::Pattern;
use crate::pyramid;

/// Search tuning values.
#[derive(Copy, Clone, Debug)]
pub struct SearchParams {
    /// The maximum number of pyramid levels.
    pub max_levels: u32,
    /// The needle's short side below which it is not shrunk further (pixels).
    pub min_level_size: u32,
    /// How far the threshold drops per level at the coarse levels.
    ///
    /// Some of the need to drop it comes from detail lost to shrinking, but the
    /// dominant cause is **downsample phase offset**. A 2x2 average is pinned to
    /// the grid of even coordinates, so when the target does not sit on that
    /// grid the pixels get grouped differently than they did when the needle was
    /// shrunk.
    ///
    /// Measured (48px pattern / 2 levels / synthetic UI scene):
    ///
    /// | target position | multiple of 4? | coarse-level score |
    /// |---|---|---|
    /// | (100, 60) | both yes | 1.000 |
    /// | (300, 120) | both yes | 1.000 |
    /// | (150, 280) | x no | 0.845 |
    /// | (10, 10) | both no | 0.650 |
    ///
    /// Real targets appear at arbitrary coordinates, so pick a value that
    /// straddles this worst case. Too small and matches are missed; too large
    /// and the candidate count grows, making refinement expensive.
    pub coarse_drop_per_level: f32,
    /// The floor for the coarse-level threshold; it never drops below this.
    pub coarse_floor: f32,
    /// How many candidates are carried from the coarse levels into refinement.
    pub candidates: usize,
    /// The margin taken around a candidate during refinement (pixels).
    ///
    /// Coordinates double per level, so at least 2 is required. This is what is
    /// added on top of that.
    pub refine_margin: u32,
    /// Whether to brute-force at full size when the pyramid search returns
    /// nothing.
    ///
    /// The pyramid search assumes the target keeps its shape when shrunk.
    /// Patterns made only of high-frequency content (fine noise-like textures,
    /// shapes drawn from 1px rules) vanish when shrunk, so no candidate survives
    /// the coarse stage.
    ///
    /// A miss silently breaks an automation script, so the default is to check
    /// again at full size. The cost is that a genuine "not found" becomes as
    /// expensive as a brute-force search. Turn this off only when you do not
    /// want to pay that on every `wait` poll.
    pub full_search_fallback: bool,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            max_levels: 3,
            min_level_size: 12,
            // The measured worst-case phase offset is 0.35 over 2 levels =
            // 0.175; this leaves headroom above that.
            coarse_drop_per_level: 0.20,
            coarse_floor: 0.30,
            // The refinement window is small, so more candidates costs almost
            // nothing. A miss costs far more.
            candidates: 32,
            refine_margin: 3,
            full_search_fallback: true,
        }
    }
}

/// One hit, in full-size haystack coordinates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Hit {
    pub x: u32,
    pub y: u32,
    pub score: f32,
}

/// Whether matching runs on the GPU or the CPU.
///
/// The CPU path keeps the tool usable where no GPU is available (CI without a
/// GPU, remote desktop sessions and so on). It selects the f64 direct kernel
/// for small templates and a GPU-compatible block-FFT ZMD kernel for larger
/// ones.
pub enum Matcher {
    Gpu(Box<TemplateMatcher>),
    Cpu(Box<FastCpuMatcher>),
}

impl Matcher {
    /// Try the GPU, falling back to the CPU.
    pub fn best_available() -> Self {
        match TemplateMatcher::new() {
            Ok(m) => {
                log::info!("matching: GPU ({})", m.adapter_info().name);
                Matcher::Gpu(Box::new(m))
            }
            Err(e) => {
                log::warn!("cannot initialise the GPU, using the CPU implementation: {e}");
                Self::cpu()
            }
        }
    }

    /// Construct the production CPU backend explicitly.
    pub fn cpu() -> Self {
        Matcher::Cpu(Box::new(FastCpuMatcher::new()))
    }

    pub fn backend_name(&self) -> &'static str {
        match self {
            Matcher::Gpu(_) => "gpu",
            Matcher::Cpu(_) => "cpu-fast",
        }
    }

    /// Return the full-size score map as-is.
    ///
    /// Used to build a heat map on failure. It is a brute-force pass, so only
    /// pay for it on failure.
    pub fn score_map(
        &mut self,
        haystack: &Image<'_>,
        needle: &Image<'_>,
    ) -> Option<Image<'static>> {
        self.run(haystack, needle)
    }

    fn run(&mut self, haystack: &Image<'_>, needle: &Image<'_>) -> Option<Image<'static>> {
        match self {
            Matcher::Gpu(m) => m
                .match_template(haystack, needle, MatchMethod::ZeroMeanDice)
                .map_err(|e| log::warn!("GPU matching failed: {e}"))
                .ok(),
            Matcher::Cpu(m) => m.match_template(haystack, needle),
        }
    }
}

/// Build the haystack pyramid.
///
/// The haystack changes every frame so it cannot be cached. The needle side
/// ([`Pattern`]) reuses the pyramid built at construction time.
pub fn build_haystack_pyramid(haystack: &Image<'_>, levels: u32) -> Vec<Image<'static>> {
    pyramid::build(haystack, levels)
}

/// Decide how many pyramid levels are usable.
///
/// Capped by the needle's level count, checking at each level that the haystack
/// is larger than the needle.
fn usable_levels(haystack: (u32, u32), pattern: &Pattern) -> u32 {
    let mut levels = pattern.max_level();
    while levels > 0 {
        let n = pattern.level(levels);
        let hw = haystack.0 >> levels;
        let hh = haystack.1 >> levels;
        if hw >= n.width && hh >= n.height {
            break;
        }
        levels -= 1;
    }
    levels
}

/// Search for a pattern.
///
/// When `want_all` is true, every non-overlapping match is returned (the
/// equivalent of `findAll`). When false, only the best-scoring one.
///
/// The returned coordinates are in the full-size haystack.
pub fn find(
    matcher: &mut Matcher,
    haystack_levels: &[Image<'static>],
    pattern: &Pattern,
    params: &SearchParams,
    want_all: bool,
) -> Vec<Hit> {
    let base = &haystack_levels[0];
    let levels =
        usable_levels((base.width, base.height), pattern).min(haystack_levels.len() as u32 - 1);

    if levels == 0 {
        return full_search(
            matcher,
            base,
            pattern.level(0),
            pattern.similarity(),
            want_all,
        );
    }

    // --- coarse search ---
    //
    // For findAll, drop all the way to the floor.
    //
    // A single find only needs one hit, so a somewhat tight threshold still lets
    // something through, and if everything is missed the empty result triggers
    // the fallback. findAll, however, can **miss only some of them**: the result
    // is not empty, the fallback never fires, and the count silently shrinks.
    //
    // Refinement only looks at a small window per candidate and NMS caps the
    // candidate count, so lowering this costs little. A miss costs more.
    let coarse_threshold = if want_all {
        params.coarse_floor
    } else {
        (pattern.similarity() - params.coarse_drop_per_level * levels as f32)
            .max(params.coarse_floor)
    };
    let coarse = full_search(
        matcher,
        &haystack_levels[levels as usize],
        pattern.level(levels),
        coarse_threshold,
        // Collect several candidates even when this is not findAll: the best
        // at a coarse level is not necessarily the best at full size.
        true,
    );

    if coarse.is_empty() {
        return fallback(
            matcher,
            base,
            pattern,
            params,
            want_all,
            "no candidate from the coarse search",
        );
    }

    let mut candidates: Vec<Hit> = coarse;
    candidates.truncate(if want_all {
        params.candidates.max(64)
    } else {
        params.candidates
    });

    // --- refinement ---
    for level in (0..levels).rev() {
        let hay = &haystack_levels[level as usize];
        let needle = pattern.level(level);
        // Coordinates double per level, so the window covers that error (up to
        // 1 pixel, which becomes 2 after doubling) plus the margin. The window
        // size is constant across candidates so the matcher's GPU buffers are
        // not reallocated.
        let margin = 2 + params.refine_margin;
        let win_w = needle.width + margin * 2;
        let win_h = needle.height + margin * 2;

        let mut refined = Vec::with_capacity(candidates.len());
        for cand in &candidates {
            let cx = (cand.x * 2) as i32 - margin as i32;
            let cy = (cand.y * 2) as i32 - margin as i32;

            let Some((window, ox, oy)) = pyramid::crop_clamped(hay, cx, cy, win_w, win_h) else {
                // The window is larger than the haystack. Fall back to a full
                // search at this level.
                let hits = full_search(matcher, hay, needle, 0.0, false);
                refined.extend(hits);
                continue;
            };

            let Some(scores) = matcher.run(&window, needle) else {
                continue;
            };
            let e = find_extremes(&scores);
            refined.push(Hit {
                x: ox + e.max_value_location.0,
                y: oy + e.max_value_location.1,
                score: e.max_value,
            });
        }
        candidates = refined;
    }

    // --- finish ---
    candidates.retain(|h| h.score >= pattern.similarity());
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if candidates.is_empty() {
        return fallback(
            matcher,
            base,
            pattern,
            params,
            want_all,
            "refinement did not clear the threshold",
        );
    }

    if want_all {
        dedupe_overlapping(candidates, (pattern.width(), pattern.height()))
    } else {
        candidates.truncate(1);
        candidates
    }
}

/// Re-check at full size when the pyramid search came back empty.
fn fallback(
    matcher: &mut Matcher,
    base: &Image<'_>,
    pattern: &Pattern,
    params: &SearchParams,
    want_all: bool,
    reason: &str,
) -> Vec<Hit> {
    if !params.full_search_fallback {
        return Vec::new();
    }
    let hits = full_search(
        matcher,
        base,
        pattern.level(0),
        pattern.similarity(),
        want_all,
    );
    if !hits.is_empty() {
        // Reaching here means the pattern does not survive shrinking (it holds
        // only high-frequency content). Leave a hint for tuning rather than
        // silently getting slower.
        log::debug!(
            "'{}': {reason}, but {} found at full size. \
             The pattern does not survive shrinking, so the pyramid search is not helping",
            pattern.name(),
            hits.len()
        );
    }
    hits
}

/// Brute-force a whole image and return the matches at or above the threshold.
fn full_search(
    matcher: &mut Matcher,
    haystack: &Image<'_>,
    needle: &Image<'_>,
    threshold: f32,
    want_all: bool,
) -> Vec<Hit> {
    let Some(scores) = matcher.run(haystack, needle) else {
        return Vec::new();
    };

    if !want_all {
        let e = find_extremes(&scores);
        if e.max_value < threshold {
            return Vec::new();
        }
        return vec![Hit {
            x: e.max_value_location.0,
            y: e.max_value_location.1,
            score: e.max_value,
        }];
    }

    let opts = NmsOptions::similar(threshold).with_max_results(256);
    find_matches(&scores, (needle.width, needle.height), &opts)
        .into_iter()
        .map(|m| Hit {
            x: m.x,
            y: m.y,
            score: m.score,
        })
        .collect()
}

/// Merge candidates that converged on the same target during refinement.
///
/// Two candidates that were neighbours at a coarse level can settle on the same
/// position once refined. Walk them in score order and drop any that overlaps a
/// rectangle already kept.
fn dedupe_overlapping(hits: Vec<Hit>, size: (u32, u32)) -> Vec<Hit> {
    let mut kept: Vec<Hit> = Vec::new();
    for hit in hits {
        let overlaps = kept.iter().any(|k| {
            let dx = (k.x as i64 - hit.x as i64).abs();
            let dy = (k.y as i64 - hit.y as i64).abs();
            dx < size.0 as i64 && dy < size.1 as i64
        });
        if !overlaps {
            kept.push(hit);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic synthetic UI scene.
    ///
    /// **This must not be white noise.** Per-pixel independent random values
    /// hold only high-frequency content, and shrinking erases them. The pyramid
    /// search assumes the target keeps its shape when shrunk, and real GUIs
    /// (filled areas, rectangles, text) have structure at several scales. The
    /// test scene matches that.
    fn scene(w: u32, h: u32, seed: u64) -> Image<'static> {
        let mut state = seed | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut data = vec![0.88f32; (w * h) as usize];

        // Large filled areas (low frequency).
        for _ in 0..24 {
            let x0 = (next() % u64::from(w)) as u32;
            let y0 = (next() % u64::from(h)) as u32;
            let bw = 24 + (next() % 90) as u32;
            let bh = 18 + (next() % 70) as u32;
            let shade = 0.1 + (next() % 700) as f32 / 1000.0;
            for y in y0..(y0 + bh).min(h) {
                for x in x0..(x0 + bw).min(w) {
                    data[(y * w + x) as usize] = shade;
                }
            }
        }

        // Mid-scale structure (borders).
        for _ in 0..30 {
            let x0 = (next() % u64::from(w)) as u32;
            let y0 = (next() % u64::from(h)) as u32;
            let bw = (8 + (next() % 40) as u32).min(w - x0.min(w - 1) - 1).max(2);
            let bh = (8 + (next() % 30) as u32).min(h - y0.min(h - 1) - 1).max(2);
            let shade = (next() % 400) as f32 / 1000.0;
            for x in x0..(x0 + bw).min(w) {
                data[(y0 * w + x) as usize] = shade;
                data[(((y0 + bh - 1).min(h - 1)) * w + x) as usize] = shade;
            }
            for y in y0..(y0 + bh).min(h) {
                data[(y * w + x0) as usize] = shade;
                data[(y * w + (x0 + bw - 1).min(w - 1)) as usize] = shade;
            }
        }

        // Fine speckle (high frequency). In small amounts it does not dominate
        // after shrinking.
        for _ in 0..(w as u64 * h as u64 / 300) {
            let x = (next() % u64::from(w)) as u32;
            let y = (next() % u64::from(h)) as u32;
            data[(y * w + x) as usize] = (next() % 1000) as f32 / 1000.0;
        }

        Image::new(data, w, h)
    }

    /// A scene with only high-frequency content, reproducing the conditions
    /// where the pyramid search does not work.
    fn white_noise(w: u32, h: u32, seed: u64) -> Image<'static> {
        let mut state = seed | 1;
        let mut data = vec![0.0f32; (w * h) as usize];
        for v in data.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *v = 0.3 + (state % 500) as f32 / 1000.0;
        }
        Image::new(data, w, h)
    }

    fn crop(src: &Image<'_>, x: u32, y: u32, w: u32, h: u32) -> Image<'static> {
        pyramid::crop_clamped(src, x as i32, y as i32, w, h)
            .unwrap()
            .0
    }

    fn params() -> SearchParams {
        SearchParams::default()
    }

    #[test]
    fn pyramid_search_finds_exact_patch() {
        let hay = scene(512, 384, 7);
        let needle_img = crop(&hay, 200, 150, 64, 64);
        let pattern = Pattern::from_image(needle_img, "n", 0.9, 3, 12);
        assert_eq!(pattern.max_level(), 2, "64px should give 2 levels");

        let mut matcher = Matcher::cpu();
        let levels = build_haystack_pyramid(&hay, pattern.max_level());
        let hits = find(&mut matcher, &levels, &pattern, &params(), false);

        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!((hits[0].x, hits[0].y), (200, 150));
        assert!(hits[0].score > 0.99, "{}", hits[0].score);
    }

    #[test]
    fn pyramid_search_agrees_with_full_search() {
        let hay = scene(400, 300, 11);
        let needle_img = crop(&hay, 97, 61, 48, 48);
        let pattern = Pattern::from_image(needle_img.clone(), "n", 0.8, 3, 12);

        let mut matcher = Matcher::cpu();
        let levels = build_haystack_pyramid(&hay, pattern.max_level());
        let pyr = find(&mut matcher, &levels, &pattern, &params(), false);
        let full = full_search(&mut matcher, &hay, &needle_img, 0.8, false);

        assert_eq!(pyr.len(), 1, "{pyr:?}");
        assert_eq!(full.len(), 1);
        assert_eq!((pyr[0].x, pyr[0].y), (full[0].x, full[0].y));
    }

    #[test]
    fn small_pattern_skips_the_pyramid() {
        let hay = scene(200, 200, 13);
        let needle_img = crop(&hay, 30, 40, 16, 16);
        let pattern = Pattern::from_image(needle_img, "n", 0.9, 3, 12);
        assert_eq!(
            pattern.max_level(),
            0,
            "no pyramid should be built for 16px"
        );

        let mut matcher = Matcher::cpu();
        let levels = build_haystack_pyramid(&hay, 0);
        let hits = find(&mut matcher, &levels, &pattern, &params(), false);
        assert_eq!((hits[0].x, hits[0].y), (30, 40));
    }

    /// The fallback fires when the coarse search produces no candidate at all.
    ///
    /// The coarse threshold is pinned at 0.999 to make the coarse search miss on
    /// purpose. The real-data conditions for a miss (a pattern with only
    /// high-frequency content, say) depend on the content and are hard to
    /// reproduce, so the threshold is used to exercise the mechanism reliably.
    #[test]
    fn falls_back_to_full_resolution_when_coarse_finds_nothing() {
        let hay = white_noise(384, 256, 29);
        // Use coordinates that are not multiples of 4. When aligned, even the
        // coarse level scores 1.0, which clears the 0.999 threshold below and
        // the miss is not reproduced.
        let needle_img = crop(&hay, 122, 82, 64, 64);
        let pattern = Pattern::from_image(needle_img, "noise", 0.9, 3, 12);
        assert!(pattern.max_level() > 0, "a pyramid is assumed to be built");

        let mut matcher = Matcher::cpu();
        let levels = build_haystack_pyramid(&hay, pattern.max_level());

        // The phase offset keeps the coarse score below 1.0, so this always misses.
        let strict_coarse = SearchParams {
            coarse_drop_per_level: 0.0,
            coarse_floor: 0.999,
            ..params()
        };

        let hits = find(&mut matcher, &levels, &pattern, &strict_coarse, false);
        assert_eq!(hits.len(), 1, "the fallback did not fire: {hits:?}");
        assert_eq!((hits[0].x, hits[0].y), (122, 82));

        // Turn it off and the match is missed. That difference is why the
        // fallback exists.
        let no_fallback = SearchParams {
            full_search_fallback: false,
            ..strict_coarse
        };
        let hits = find(&mut matcher, &levels, &pattern, &no_fallback, false);
        assert!(
            hits.is_empty(),
            "found even with the fallback disabled: {hits:?}"
        );
    }

    /// **A "ghost" hidden below quantization must not be called a match.**
    ///
    /// A regression for a bug hit in a real application. The symptom was "when
    /// the image being searched for is not on screen, a score of 1.0 appears
    /// somewhere empty; the place changes every time and it sometimes does not
    /// reproduce".
    ///
    /// The root cause is NCC's contrast invariance: NCC returns 1.0 even for a
    /// copy of the template flattened to an amplitude smaller than one 8-bit
    /// step. A place that looks perfectly blank on screen claims to be an exact
    /// match. This was originally patched with a uniform-window guard, but is
    /// now gone at the root thanks to the move to ZMD
    /// (`MatchMethod::ZeroMeanDice`). Under ZMD a ghost only scores
    /// s ≈ 2g/(1+g²), i.e. the order of the amplitude ratio (~8e-4 here). See
    /// docs/architecture/matching.md for the design history.
    ///
    /// It goes through the search path (cropping a small window per candidate
    /// and re-matching) to confirm the property holds along the same route used
    /// in production. The old NCC implementation without the guard fails at
    /// score 0.99999535 even on the CPU path.
    #[test]
    fn sub_quantization_ghost_is_not_a_match() {
        let needle = crop(&scene(256, 256, 91), 40, 40, 64, 64);

        // Place a copy of the needle on a uniform bright background at one
        // tenth of an 8-bit step. That is faint enough to vanish under 8-bit
        // quantization, and looks blank to a human and to a capture alike.
        let bg = 0.97f32;
        let amp = (1.0 / 255.0) / 10.0;
        let n_mean = needle.data.iter().sum::<f32>() / needle.data.len() as f32;

        let (w, h) = (512usize, 384);
        let mut pixels = vec![bg; w * h];
        let at = (200usize, 150);
        for j in 0..needle.height as usize {
            for i in 0..needle.width as usize {
                let d = needle.data[j * needle.width as usize + i] - n_mean;
                pixels[(at.1 + j) * w + at.0 + i] = bg + d * amp;
            }
        }
        let hay = Image::new(pixels, w as u32, h as u32);

        let pattern = Pattern::from_image(needle, "ghost", 0.9, 3, 12);
        let levels = build_haystack_pyramid(&hay, pattern.max_level());

        // Check both CPU and GPU: the score formula lives in both implementations.
        let mut backends = vec![Matcher::cpu()];
        match TemplateMatcher::new() {
            Ok(m) => backends.push(Matcher::Gpu(Box::new(m))),
            Err(e) => eprintln!("no GPU available, checking the CPU only: {e}"),
        }

        for mut matcher in backends {
            let name = matcher.backend_name();
            let hits = find(&mut matcher, &levels, &pattern, &params(), true);
            assert!(
                hits.is_empty(),
                "{name}: a sub-quantization ghost was judged a match: {hits:?}"
            );
        }
    }

    /// Downsample phase offset costs score at the coarse levels.
    ///
    /// This is the property `coarse_drop_per_level` rests on, so it is pinned
    /// here to make a change in the numbers visible.
    #[test]
    fn downsample_phase_misalignment_costs_score() {
        let hay = scene(512, 384, 17);
        let patch = crop(&hay, 100, 60, 48, 48); // coordinates that are multiples of 4
        let pattern = Pattern::from_image(patch, "aligned", 0.5, 3, 12);
        let levels = build_haystack_pyramid(&hay, pattern.max_level());
        let mut matcher = Matcher::cpu();

        let coarse = full_search(&mut matcher, &levels[2], pattern.level(2), 0.3, true);
        let aligned = coarse
            .iter()
            .find(|h| (h.x, h.y) == (25, 15))
            .expect("the aligned position was not found");
        assert!(
            aligned.score > 0.999,
            "should be 1.0 when aligned: {}",
            aligned.score
        );

        // Shifting the same pattern by 2 pixels makes it look different at the
        // coarse level.
        let shifted = crop(&hay, 102, 62, 48, 48);
        let pattern2 = Pattern::from_image(shifted, "shifted", 0.5, 3, 12);
        let coarse2 = full_search(&mut matcher, &levels[2], pattern2.level(2), 0.3, true);
        let best = coarse2.first().expect("no candidate");
        assert!(
            best.score < 0.999,
            "the phase offset did not cost any score: {}",
            best.score
        );
    }

    #[test]
    fn find_all_recovers_repeated_patches() {
        let mut hay = scene(512, 384, 17);
        let patch = crop(&hay, 10, 10, 48, 48);

        // Paste the same patch in three places.
        let positions = [(100u32, 60u32), (300, 120), (150, 280)];
        {
            let data = hay.data.to_mut();
            for &(px, py) in &positions {
                for row in 0..48usize {
                    let dst = (py as usize + row) * 512 + px as usize;
                    let src = row * 48;
                    data[dst..dst + 48].copy_from_slice(&patch.data[src..src + 48]);
                }
            }
        }

        let pattern = Pattern::from_image(patch, "n", 0.95, 3, 12);
        let mut matcher = Matcher::cpu();
        let levels = build_haystack_pyramid(&hay, pattern.max_level());

        let hits = find(&mut matcher, &levels, &pattern, &params(), true);

        let mut found: Vec<(u32, u32)> = hits.iter().map(|h| (h.x, h.y)).collect();
        found.sort_unstable();
        // The original at (10,10) matches too, so there are 4.
        let mut want: Vec<(u32, u32)> = positions.to_vec();
        want.push((10, 10));
        want.sort_unstable();
        assert_eq!(found, want, "hits = {hits:?}");
    }

    #[test]
    fn absent_pattern_yields_nothing() {
        let hay = scene(300, 300, 23);
        let other = scene(64, 64, 9999); // an unrelated pattern
        let pattern = Pattern::from_image(other, "n", 0.9, 3, 12);

        let mut matcher = Matcher::cpu();
        let levels = build_haystack_pyramid(&hay, pattern.max_level());
        let hits = find(&mut matcher, &levels, &pattern, &params(), false);
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn dedupe_keeps_only_non_overlapping() {
        let hits = vec![
            Hit {
                x: 100,
                y: 100,
                score: 0.99,
            },
            Hit {
                x: 105,
                y: 102,
                score: 0.98,
            }, // overlapping
            Hit {
                x: 200,
                y: 100,
                score: 0.97,
            },
        ];
        let kept = dedupe_overlapping(hits, (48, 48));
        assert_eq!(kept.len(), 2);
        assert_eq!((kept[0].x, kept[1].x), (100, 200));
    }
}

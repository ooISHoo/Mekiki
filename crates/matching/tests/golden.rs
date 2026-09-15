//! Golden tests. Checks the score semantics (ZMD) against externally generated
//! expectations.
//!
//! The fixtures are produced by `tools/golden/gen_golden.py`. The expected
//! values are assembled from **two paths that are separate implementations from
//! the Rust side**: the numerator from OpenCV `TM_CCOEFF` (DFT cross
//! correlation) and the denominator from a numpy integral image (f64). So this
//! is not a comparison that passes merely because the same formula was written
//! the same way twice.
//!
//! # What changed since the NCC era
//!
//! In Phase 0 the completion criterion was near-bit-exact agreement with OpenCV
//! `TM_CCOEFF_NORMED`, and that required machinery: excluded 0/0 positions
//! (unstable_indices), excluded uniform windows, and a tolerance that depended
//! on the denominator. Because the ZMD denominator is held up from below by
//! `dev2 + tn2 >= tn2 > 0`, no 0/0 exists structurally and **all of that is
//! gone**. A single flat absolute tolerance is enough. See
//! `docs/architecture/matching.md` for the history.
//!
//! Where there is no GPU, the GPU half is skipped. On CI jobs that do have a
//! GPU, set `MEKIKI_REQUIRE_GPU=1` so that a skip fails the run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mekiki_matching::{
    Image, MatchMethod, NmsOptions, TemplateMatcher, Tiling, cpu, cpu_fast::FastCpuMatcher,
    find_extremes, find_matches,
};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// manifest
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Manifest {
    opencv_version: String,
    tolerance: Tolerance,
    cases: Vec<Case>,
}

/// Tolerances, kept as separate entries per source of error.
///
/// Rolling them into a single threshold would make it impossible to tell "the
/// GPU's f32 got worse" apart from "the formula was interpreted differently
/// from the expectation".
#[derive(Deserialize)]
struct Tolerance {
    /// f64 reference implementation vs. the expectation. The difference is
    /// really the TM_CCOEFF (f32 DFT) error on the expectation side.
    zmd_abs: f64,
    /// GPU (f32) vs. the expectation. The above plus f32 accumulation.
    zmd_gpu_abs: f64,
    /// GPU (f32) vs. our own CPU (f64). Purely the f32 accumulation error.
    zmd_gpu_vs_cpu_abs: f64,
    ssd_rel: f64,
    ssd_abs: f64,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    scene: String,
    template: String,
    template_size: [u32; 2],
    planted: Vec<[u32; 2]>,
    methods: HashMap<String, Expected>,
}

#[derive(Deserialize)]
struct Expected {
    result_size: [u32; 2],
    min: f64,
    max: f64,
    min_loc: [u32; 2],
    max_loc: [u32; 2],
    samples: Samples,
    full_map: Option<String>,
}

#[derive(Deserialize)]
struct Samples {
    indices: Vec<u32>,
    values: Vec<f64>,
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("golden")
}

fn load_manifest() -> Manifest {
    let path = golden_dir().join("manifest.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read the golden data ({}): {e}\n\
             run `python tools/golden/gen_golden.py` first",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("failed to parse manifest.json")
}

fn load_gray(rel: &str) -> Image<'static> {
    let path = golden_dir().join(rel);
    let img = image::open(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .to_luma8();
    let (w, h) = img.dimensions();
    Image::from_luma8(img.as_raw(), w, h)
}

fn load_full_map(rel: &str, expected_len: usize) -> Vec<f32> {
    let bytes = std::fs::read(golden_dir().join(rel)).expect("cannot read the expected map");
    assert_eq!(
        bytes.len(),
        expected_len * 4,
        "the expected map has the wrong length"
    );
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

// ---------------------------------------------------------------------------
// comparison
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Report {
    compared: usize,
    violations: usize,
    max_abs_diff: f64,
    worst_index: usize,
    worst_pair: (f64, f64),
}

impl Report {
    fn assert_clean(&self, label: &str, limit: f64) {
        assert_eq!(
            self.violations,
            0,
            "{label}: {} of {} points exceeded the tolerance. Worst at index {} with \
             actual={:.9} expected={:.9} (diff {:.3e} > limit {limit:.3e})",
            self.violations,
            self.compared,
            self.worst_index,
            self.worst_pair.0,
            self.worst_pair.1,
            self.max_abs_diff,
        );
    }
}

/// Compare a ZMD score map against the expectation at every point, with a flat
/// absolute tolerance.
///
/// Also checks the range at every point (finite and |s| <= 1 + ε).
fn compare_zmd(actual: &[f32], expected: &[f32], limit: f64) -> Report {
    assert_eq!(actual.len(), expected.len());

    let mut report = Report::default();
    for i in 0..actual.len() {
        let v = actual[i];
        assert!(
            v.is_finite() && (-1.001..=1.001).contains(&v),
            "value out of range at index {i}: {v}"
        );

        let diff = (f64::from(v) - f64::from(expected[i])).abs();
        report.compared += 1;
        if diff > report.max_abs_diff {
            report.max_abs_diff = diff;
            report.worst_index = i;
            report.worst_pair = (f64::from(v), f64::from(expected[i]));
        }
        if diff > limit {
            report.violations += 1;
        }
    }
    report
}

/// Sampled-point comparison for ZMD. For cases that carry no full map.
fn compare_zmd_samples(actual: &Image<'_>, exp: &Expected, limit: f64, label: &str) {
    for (&idx, &want) in exp.samples.indices.iter().zip(&exp.samples.values) {
        let got = f64::from(actual.data[idx as usize]);
        assert!(
            (got - want).abs() <= limit,
            "{label}: sample {idx} does not match: actual={got:.9} expected={want:.9} \
             (diff {:.3e} > limit {limit:.3e})",
            (got - want).abs()
        );
    }
}

/// Sampled-point comparison for SAD/SSD. Their values grow with the window
/// pixel count, so a relative tolerance is used.
fn compare_ssd_samples(actual: &Image<'_>, exp: &Expected, tol: &Tolerance, label: &str) {
    for (&idx, &want) in exp.samples.indices.iter().zip(&exp.samples.values) {
        let got = f64::from(actual.data[idx as usize]);
        let limit = tol.ssd_abs + tol.ssd_rel * want.abs();
        assert!(
            (got - want).abs() <= limit,
            "{label}: sample {idx} does not match: actual={got:.9} expected={want:.9} \
             (diff {:.3e} > limit {limit:.3e})",
            (got - want).abs()
        );
    }
}

/// Check the extreme value.
///
/// When several exact matches exist (`repeated` plants the same patch multiple
/// times), which one comes out largest is decided by the lowest bits. So rather
/// than the location itself, we check that it belongs to the set of positions
/// that count as maximal on the expected map.
fn assert_extreme(
    actual: &Image<'_>,
    expected_map: Option<&[f32]>,
    exp: &Expected,
    higher_is_better: bool,
    tol: f64,
    label: &str,
) {
    let e = find_extremes(actual);
    let (got_value, got_loc) = if higher_is_better {
        (f64::from(e.max_value), e.max_value_location)
    } else {
        (f64::from(e.min_value), e.min_value_location)
    };
    let want_value = if higher_is_better { exp.max } else { exp.min };
    let want_loc = if higher_is_better {
        exp.max_loc
    } else {
        exp.min_loc
    };

    assert!(
        (got_value - want_value).abs() <= tol,
        "{label}: extreme value mismatch: actual={got_value:.9} expected={want_value:.9}"
    );

    let Some(map) = expected_map else {
        // Without a full map there is no way to resolve ties by location, so
        // only the value is checked.
        return;
    };

    let idx = (got_loc.1 as usize) * (exp.result_size[0] as usize) + got_loc.0 as usize;
    let score_there = f64::from(map[idx]);
    let tied = if higher_is_better {
        score_there >= want_value - tol
    } else {
        score_there <= want_value + tol
    };
    assert!(
        tied,
        "{label}: the extreme location {got_loc:?} only scores {score_there:.9} on the \
         expected map (expected extreme {want_value:.9} @ {want_loc:?})"
    );
}

// ---------------------------------------------------------------------------
// backends
// ---------------------------------------------------------------------------

fn try_gpu() -> Option<TemplateMatcher> {
    match TemplateMatcher::new() {
        Ok(m) => Some(m),
        Err(e) => {
            if std::env::var("MEKIKI_REQUIRE_GPU").is_ok() {
                panic!("MEKIKI_REQUIRE_GPU is set but the GPU cannot be initialised: {e}");
            }
            eprintln!("[skip] skipping the GPU tests, cannot initialise a GPU: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// the tests
// ---------------------------------------------------------------------------

/// One case's worth of ZMD checking. Only the tolerance differs between CPU and GPU.
fn check_zmd_case(backend: &str, actual: &Image<'_>, case: &Case, exp: &Expected, limit: f64) {
    let label = format!("{backend}/zmd/{}", case.name);
    assert_eq!([actual.width, actual.height], exp.result_size, "{label}");

    let expected_map = exp
        .full_map
        .as_ref()
        .map(|rel| load_full_map(rel, actual.len()));

    if let Some(map) = &expected_map {
        let report = compare_zmd(&actual.data, map, limit);
        eprintln!(
            "  {backend}/zmd {:<14} compared {:>7} points, max diff {:.3e}",
            case.name, report.compared, report.max_abs_diff
        );
        report.assert_clean(&label, limit);
    }

    compare_zmd_samples(actual, exp, limit, &label);
    assert_extreme(actual, expected_map.as_deref(), exp, true, limit, &label);
}

#[test]
fn cpu_zmd_matches_golden() {
    let manifest = load_manifest();
    eprintln!(
        "checking against golden data derived from OpenCV {}",
        manifest.opencv_version
    );

    for case in &manifest.cases {
        let exp = &case.methods["zmd"];
        let scene = load_gray(&case.scene);
        let template = load_gray(&case.template);

        let actual = cpu::match_template(&scene, &template, MatchMethod::ZeroMeanDice)
            .expect("CPU matching failed");

        check_zmd_case("cpu", &actual, case, exp, manifest.tolerance.zmd_abs);
    }
}

/// The production CPU candidate must preserve the same score contract as the
/// GPU. The f64 CPU implementation remains the stricter reference oracle; the
/// candidate is allowed no more drift than the existing GPU path.
#[test]
fn fast_cpu_zmd_matches_golden_and_reference() {
    let manifest = load_manifest();
    let mut matcher = FastCpuMatcher::new();

    for case in &manifest.cases {
        let exp = &case.methods["zmd"];
        let scene = load_gray(&case.scene);
        let template = load_gray(&case.template);
        let reference = cpu::match_template(&scene, &template, MatchMethod::ZeroMeanDice)
            .expect("reference CPU matching failed");
        let actual = matcher
            .match_template(&scene, &template)
            .expect("fast CPU matching failed");

        check_zmd_case(
            "cpu-fast",
            &actual,
            case,
            exp,
            manifest.tolerance.zmd_gpu_abs,
        );

        let report = compare_zmd(
            &actual.data,
            &reference.data,
            manifest.tolerance.zmd_gpu_vs_cpu_abs,
        );
        report.assert_clean(
            &format!("cpu-fast-vs-reference/{}", case.name),
            manifest.tolerance.zmd_gpu_vs_cpu_abs,
        );
    }
}

/// Thresholded results are the user-visible contract. A small floating-point
/// difference must not add or remove a hit around normal UI thresholds.
#[test]
fn fast_cpu_preserves_thresholded_hits() {
    let manifest = load_manifest();
    let case = manifest
        .cases
        .iter()
        .find(|c| c.name == "repeated")
        .expect("no `repeated` case");
    let scene = load_gray(&case.scene);
    let template = load_gray(&case.template);
    let reference = cpu::match_template(&scene, &template, MatchMethod::ZeroMeanDice).unwrap();
    let actual = FastCpuMatcher::new()
        .match_template(&scene, &template)
        .unwrap();

    for threshold in [0.70, 0.80, 0.90, 0.95] {
        let options = NmsOptions::similar(threshold);
        let mut expected = find_matches(&reference, tuple(case.template_size), &options);
        let mut got = find_matches(&actual, tuple(case.template_size), &options);
        expected.sort_by_key(|m| (m.y, m.x));
        got.sort_by_key(|m| (m.y, m.x));
        assert_eq!(
            got.len(),
            expected.len(),
            "hit count changed at threshold {threshold}"
        );
        for (got, expected) in got.iter().zip(&expected) {
            assert_eq!((got.x, got.y), (expected.x, expected.y));
            assert!(
                (f64::from(got.score) - f64::from(expected.score)).abs()
                    <= manifest.tolerance.zmd_gpu_vs_cpu_abs,
                "score at ({}, {}) changed from {} to {} at threshold {threshold}",
                got.x,
                got.y,
                expected.score,
                got.score,
            );
        }
    }
}

fn tuple(v: [u32; 2]) -> (u32, u32) {
    (v[0], v[1])
}

#[test]
fn gpu_zmd_matches_golden() {
    let Some(mut matcher) = try_gpu() else { return };
    let manifest = load_manifest();
    eprintln!("GPU: {}", matcher.adapter_info().name);

    // The tiled variant computes the **same values** as the naive one; only the
    // speed differs. Verifying just one of them would hide an optimisation that
    // broke the semantics.
    for tiling in [Tiling::Never, Tiling::WhenItFits] {
        eprintln!("--- tiling = {tiling:?} ---");
        matcher.set_tiling(tiling);

        for case in &manifest.cases {
            let exp = &case.methods["zmd"];
            let scene = load_gray(&case.scene);
            let template = load_gray(&case.template);

            let actual = matcher
                .match_template(&scene, &template, MatchMethod::ZeroMeanDice)
                .expect("GPU matching failed");

            check_zmd_case(
                &format!("gpu/{tiling:?}"),
                &actual,
                case,
                exp,
                manifest.tolerance.zmd_gpu_abs,
            );
        }
    }
}

/// The tiled and naive variants must produce the same values.
///
/// The comparison against the golden data has a tolerance, so on its own it
/// would miss a state where both are within tolerance but differ from each
/// other. Here they are compared directly.
#[test]
fn tiled_and_naive_shaders_agree() {
    let Some(mut matcher) = try_gpu() else { return };
    let manifest = load_manifest();

    for case in &manifest.cases {
        let scene = load_gray(&case.scene);
        let template = load_gray(&case.template);

        matcher.set_tiling(Tiling::Never);
        let naive = matcher
            .match_template(&scene, &template, MatchMethod::ZeroMeanDice)
            .unwrap();

        matcher.set_tiling(Tiling::WhenItFits);
        let tiled = matcher
            .match_template(&scene, &template, MatchMethod::ZeroMeanDice)
            .unwrap();

        assert_eq!(naive.len(), tiled.len());

        let mut worst = 0.0f64;
        let mut worst_at = 0usize;
        for i in 0..naive.len() {
            let d = (f64::from(naive.data[i]) - f64::from(tiled.data[i])).abs();
            if d > worst {
                worst = d;
                worst_at = i;
            }
        }

        eprintln!(
            "  {:<14} tpl={:>3}x{:<3} tiled={} max diff {:.3e}",
            case.name,
            case.template_size[0],
            case.template_size[1],
            mekiki_matching::tile_fits_for_test((case.template_size[0], case.template_size[1])),
            worst
        );

        // The addition order is the same, so they should be bit-identical.
        // One f32 rounding unit is allowed just in case.
        assert!(
            worst < 1.0e-6,
            "{}: index {worst_at} differs by {worst:.3e}",
            case.name
        );
    }
}

#[test]
fn ssd_matches_opencv() {
    let manifest = load_manifest();
    let mut gpu = try_gpu();

    for case in &manifest.cases {
        let exp = &case.methods["ssd"];
        let scene = load_gray(&case.scene);
        let template = load_gray(&case.template);

        let cpu_result =
            cpu::match_template(&scene, &template, MatchMethod::SumOfSquaredDifferences).unwrap();
        compare_ssd_samples(
            &cpu_result,
            exp,
            &manifest.tolerance,
            &format!("cpu/ssd/{}", case.name),
        );

        if let Some(matcher) = gpu.as_mut() {
            let gpu_result = matcher
                .match_template(&scene, &template, MatchMethod::SumOfSquaredDifferences)
                .unwrap();
            compare_ssd_samples(
                &gpu_result,
                exp,
                &manifest.tolerance,
                &format!("gpu/ssd/{}", case.name),
            );
        }
    }
}

/// Cross-check between GPU and CPU.
///
/// When something disagrees with the golden data, this is the evidence that
/// separates "the algorithm is wrong" from "f32 rounding". SAD has no external
/// oracle, so this is the only path that verifies it at all.
///
/// The 0/0 and uniform-window exclusions that existed in the NCC era are gone;
/// ZMD can be compared at every point.
#[test]
fn gpu_agrees_with_cpu() {
    let Some(mut matcher) = try_gpu() else { return };
    let manifest = load_manifest();

    for case in &manifest.cases {
        let scene = load_gray(&case.scene);
        let template = load_gray(&case.template);

        for method in [
            MatchMethod::ZeroMeanDice,
            MatchMethod::SumOfSquaredDifferences,
            MatchMethod::SumOfAbsoluteDifferences,
        ] {
            let g = matcher.match_template(&scene, &template, method).unwrap();
            let c = cpu::match_template(&scene, &template, method).unwrap();
            assert_eq!(g.len(), c.len());

            let scale = if method.higher_is_better() {
                1.0
            } else {
                // SAD/SSD values grow with the window pixel count, so compare
                // relatively.
                f64::from(case.template_size[0]) * f64::from(case.template_size[1])
            };

            let mut worst = 0.0f64;
            let mut worst_at = 0usize;
            for i in 0..g.len() {
                let d = (f64::from(g.data[i]) - f64::from(c.data[i])).abs() / scale;
                if d > worst {
                    worst = d;
                    worst_at = i;
                }
            }

            eprintln!(
                "  gpu-vs-cpu {:<14} {:<28} max diff {:.3e}",
                case.name,
                format!("{method:?}"),
                worst
            );
            assert!(
                worst < manifest.tolerance.zmd_gpu_vs_cpu_abs,
                "{}/{:?}: GPU and CPU differ by {worst:.3e} at index {worst_at}",
                case.name,
                method
            );
        }
    }
}

/// The findAll equivalent. Recovers all five planted positions and returns
/// nothing extra.
#[test]
fn findall_recovers_planted_positions() {
    let manifest = load_manifest();
    let case = manifest
        .cases
        .iter()
        .find(|c| c.name == "repeated")
        .expect("no `repeated` case");

    let scene = load_gray(&case.scene);
    let template = load_gray(&case.template);
    let scores = cpu::match_template(&scene, &template, MatchMethod::ZeroMeanDice).unwrap();

    let hits = find_matches(
        &scores,
        (case.template_size[0], case.template_size[1]),
        &NmsOptions::similar(0.95),
    );

    assert_eq!(
        hits.len(),
        case.planted.len(),
        "detection count differs from the number planted: {hits:?}"
    );

    let mut found: Vec<(u32, u32)> = hits.iter().map(|m| (m.x, m.y)).collect();
    let mut want: Vec<(u32, u32)> = case.planted.iter().map(|p| (p[0], p[1])).collect();
    found.sort_unstable();
    want.sort_unstable();
    assert_eq!(found, want);

    for hit in &hits {
        assert!(hit.score > 0.99, "a planted position scored low: {hit:?}");
    }
}

/// A uniform template "matches nothing" — an all-zero map.
///
/// OpenCV fills the map with 1.0, but once the compatibility requirement was
/// dropped this was changed to the answer that is correct for UI search. In
/// practice the load-time check in `Pattern` (`Error::FlatPattern`) rejects it
/// first, so this verifies the matcher's own defence.
#[test]
fn flat_template_scores_zero() {
    let manifest = load_manifest();
    let case = manifest
        .cases
        .iter()
        .find(|c| c.name == "flat_template")
        .expect("no `flat_template` case");

    let exp = &case.methods["zmd"];
    assert_eq!(exp.min, 0.0, "the golden data's premise has changed");
    assert_eq!(exp.max, 0.0);

    let scene = load_gray(&case.scene);
    let template = load_gray(&case.template);

    let c = cpu::match_template(&scene, &template, MatchMethod::ZeroMeanDice).unwrap();
    assert!(
        c.data.iter().all(|&v| v == 0.0),
        "the CPU side is not filled with 0.0"
    );

    if let Some(mut matcher) = try_gpu() {
        let g = matcher
            .match_template(&scene, &template, MatchMethod::ZeroMeanDice)
            .unwrap();
        assert!(
            g.data.iter().all(|&v| v == 0.0),
            "the GPU side is not filled with 0.0"
        );
    }
}

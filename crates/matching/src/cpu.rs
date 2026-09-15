//! CPU reference implementation.
//!
//! It exists for two reasons.
//!
//! 1. Cross-checking the GPU implementation. The GPU accumulates in f32 and
//!    this one in f64, which lets us tell "the difference from the golden data
//!    is f32 rounding" apart from "the algorithm is wrong".
//! 2. The small-template production kernel, where FFT setup costs more than a
//!    direct scan.
//!
//! It is a naive O(W*H*w*h) implementation, parallelised across rows with
//! rayon and nothing more. Large-template production searches use
//! [`crate::cpu_fast`], while this implementation remains the accuracy oracle.

use rayon::prelude::*;

use crate::prepare::FLAT_TEMPLATE_EPS;
use crate::{Image, MatchMethod, result_size};

/// Run template matching on the CPU.
///
/// Returns `None` if the template is larger than the input.
/// Score semantics are identical to the GPU implementation (see
/// `MatchMethod::ZeroMeanDice` for the definition of ZMD).
pub fn match_template(
    input: &Image<'_>,
    template: &Image<'_>,
    method: MatchMethod,
) -> Option<Image<'static>> {
    let (result_width, result_height) = result_size(
        (input.width, input.height),
        (template.width, template.height),
    )?;

    let iw = input.width as usize;
    let tw = template.width as usize;
    let th = template.height as usize;
    let area = (tw * th) as f64;

    // ZMD needs the zero-meaned template and the template-side energy of the
    // denominator up front. Same steps as the GPU path.
    let (tpl, tn2) = match method {
        MatchMethod::ZeroMeanDice => {
            let sum: f64 = template.data.iter().map(|&v| f64::from(v)).sum();
            let mean = sum / area;
            let centered: Vec<f64> = template.data.iter().map(|&v| f64::from(v) - mean).collect();
            let tn2 = centered.iter().map(|v| v * v).sum::<f64>();
            (centered, tn2)
        }
        _ => (template.data.iter().map(|&v| f64::from(v)).collect(), 0.0),
    };

    // A uniform template is the only case where the ZMD denominator (tn2) is 0.
    // The load-time check in the layer above rejects those first; this is a
    // defence that returns an all-zero map (same as the GPU path).
    // The threshold matches the GPU side deliberately. Deciding this at f64
    // precision here alone would make CPU and GPU disagree for near-uniform
    // templates.
    if method == MatchMethod::ZeroMeanDice
        && tn2 < f64::from(FLAT_TEMPLATE_EPS) * f64::from(FLAT_TEMPLATE_EPS)
    {
        return Some(Image::new(
            vec![0.0f32; (result_width as usize) * (result_height as usize)],
            result_width,
            result_height,
        ));
    }

    let mut data = vec![0.0f32; (result_width as usize) * (result_height as usize)];

    data.par_chunks_mut(result_width as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                *out = score_at(&input.data, iw, &tpl, tw, th, tn2, area, x, y, method);
            }
        });

    Some(Image::new(data, result_width, result_height))
}

#[allow(clippy::too_many_arguments)]
fn score_at(
    input: &[f32],
    input_width: usize,
    tpl: &[f64],
    tw: usize,
    th: usize,
    tn2: f64,
    area: f64,
    x: usize,
    y: usize,
    method: MatchMethod,
) -> f32 {
    match method {
        MatchMethod::SumOfAbsoluteDifferences => {
            let mut total = 0.0f64;
            for j in 0..th {
                let in_row = (y + j) * input_width + x;
                let tp_row = j * tw;
                for i in 0..tw {
                    total += (f64::from(input[in_row + i]) - tpl[tp_row + i]).abs();
                }
            }
            total as f32
        }
        MatchMethod::SumOfSquaredDifferences => {
            let mut total = 0.0f64;
            for j in 0..th {
                let in_row = (y + j) * input_width + x;
                let tp_row = j * tw;
                for i in 0..tw {
                    let d = f64::from(input[in_row + i]) - tpl[tp_row + i];
                    total += d * d;
                }
            }
            total as f32
        }
        MatchMethod::ZeroMeanDice => {
            let mut sum_i = 0.0f64;
            let mut sum_i2 = 0.0f64;
            let mut sum_it = 0.0f64;
            for j in 0..th {
                let in_row = (y + j) * input_width + x;
                let tp_row = j * tw;
                for i in 0..tw {
                    let v = f64::from(input[in_row + i]);
                    sum_i += v;
                    sum_i2 += v * v;
                    sum_it += v * tpl[tp_row + i];
                }
            }

            let num = sum_it;
            let dev2 = (sum_i2 - sum_i * sum_i / area).max(0.0);

            // ZMD. The denominator is held up from below by tn2 > 0, so there
            // is no 0/0, no uniform-window guard and no clamping branch.
            // |s| <= 1 follows from Cauchy-Schwarz; the clamp only catches
            // rounding overshoot. See the shader (`main_zmd`) for the reasoning.
            let s = 2.0 * num / (dev2 + tn2);
            s.clamp(-1.0, 1.0) as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> Image<'static> {
        // A 6x6 gradient with a bright 2x2 block placed at (3,1).
        let mut data = vec![0.0f32; 36];
        for y in 0..6 {
            for x in 0..6 {
                data[y * 6 + x] = (x + y) as f32 * 0.01;
            }
        }
        data[6 + 3] = 0.9;
        data[6 + 4] = 0.1;
        data[12 + 3] = 0.2;
        data[12 + 4] = 0.8;
        Image::new(data, 6, 6)
    }

    #[test]
    fn zmd_finds_exact_patch_with_score_one() {
        let s = scene();
        let t = Image::new(vec![0.9, 0.1, 0.2, 0.8], 2, 2);
        let r = match_template(&s, &t, MatchMethod::ZeroMeanDice).unwrap();
        let e = crate::find_extremes(&r);
        assert_eq!(e.max_value_location, (3, 1));
        assert!((e.max_value - 1.0).abs() < 1e-5, "score = {}", e.max_value);
    }

    /// Pins the contrast sensitivity of ZMD. **A deliberate difference from NCC.**
    ///
    /// Scaling the template by a gain of a (plus an offset) keeps the location
    /// but drops the score to p(g) = 2g/(1+g²) with g = 1/a. NCC would keep
    /// returning 1.0 here, and that invariance was the root of the
    /// "blank areas claim a perfect match" false positives.
    #[test]
    fn zmd_penalizes_contrast_mismatch_by_p_of_g() {
        let s = scene();
        // The same pattern scaled by 2.5 with 0.3 added. g = 1/2.5 = 0.4.
        let t = Image::new(
            vec![
                0.9 * 2.5 + 0.3,
                0.1 * 2.5 + 0.3,
                0.2 * 2.5 + 0.3,
                0.8 * 2.5 + 0.3,
            ],
            2,
            2,
        );
        let r = match_template(&s, &t, MatchMethod::ZeroMeanDice).unwrap();
        let e = crate::find_extremes(&r);
        assert_eq!(e.max_value_location, (3, 1));
        let g = 1.0 / 2.5f32;
        let expected = 2.0 * g / (1.0 + g * g); // = 0.68965...
        assert!(
            (e.max_value - expected).abs() < 1e-5,
            "score = {} (expected {expected})",
            e.max_value
        );
    }

    /// The contrast that motivates ZMD over SAD/SSD.
    ///
    /// ZMD is invariant to a brightness **offset** (both window and template
    /// have their mean removed). Unlike a contrast gain, an offset does not
    /// change the variance, so g stays at 1.
    ///
    /// Phrasing this as "the location of the minimum moves" is unreliable: on a
    /// small synthetic scene it can stay put by chance (and it did). The claim
    /// is not about location but about whether the score at the correct
    /// location degrades under a brightness shift, so we look at that directly.
    #[test]
    fn ssd_degrades_under_brightness_shift_while_zmd_does_not() {
        let s = scene();
        let exact = Image::new(vec![0.9, 0.1, 0.2, 0.8], 2, 2);
        let shifted = Image::new(vec![0.9 + 0.3, 0.1 + 0.3, 0.2 + 0.3, 0.8 + 0.3], 2, 2);

        let ssd_exact = match_template(&s, &exact, MatchMethod::SumOfSquaredDifferences)
            .unwrap()
            .get(3, 1);
        let ssd_shifted = match_template(&s, &shifted, MatchMethod::SumOfSquaredDifferences)
            .unwrap()
            .get(3, 1);

        assert!(
            ssd_exact < 1e-6,
            "exact match yet SSD is not 0: {ssd_exact}"
        );
        assert!(
            ssd_shifted > 0.3,
            "SSD did not degrade under the brightness shift: {ssd_shifted}"
        );

        // Feeding the same shift to ZMD still gives 1.0.
        let zmd_shifted = match_template(&s, &shifted, MatchMethod::ZeroMeanDice)
            .unwrap()
            .get(3, 1);
        assert!(
            (zmd_shifted - 1.0).abs() < 1e-5,
            "ZMD broke under the brightness shift: {zmd_shifted}"
        );
    }

    /// A uniform template "matches nothing" — an all-zero map.
    ///
    /// OpenCV fills the map with 1.0, but once the compatibility requirement
    /// was dropped this was changed to the answer that is correct for UI
    /// search. Normally the load-time check in the layer above rejects it first.
    #[test]
    fn flat_template_yields_all_zeros() {
        let s = scene();
        let t = Image::new(vec![0.5; 4], 2, 2);
        let r = match_template(&s, &t, MatchMethod::ZeroMeanDice).unwrap();
        assert!(r.data.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn template_larger_than_input_is_none() {
        let s = Image::new(vec![0.0; 4], 2, 2);
        let t = Image::new(vec![0.0; 9], 3, 3);
        assert!(match_template(&s, &t, MatchMethod::ZeroMeanDice).is_none());
    }
}

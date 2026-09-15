//! Pre-processing applied before handing data to the GPU.
//!
//! This is where the numerical conditioning of ZMD is concentrated. See the
//! comments on [`prepare_template`] / [`center_input`] for the details.

use crate::{Image, MatchMethod};

/// A template, ready to be uploaded to the GPU.
pub(crate) struct PreparedTemplate {
    pub data: Vec<f32>,
    /// `sqrt(sum((T - mean(T))^2))`, used in the ZMD denominator. It is squared
    /// before being passed to the shader. 0.0 for SAD/SSD.
    pub norm: f32,
}

/// Pre-process a template according to the match method.
///
/// For ZMD the template is zero-meaned before being sent. That makes
/// `sum(T') = 0`, so the shader's numerator is just `sum(I * T')` and there is
/// no term involving the window mean to subtract. One fewer subtraction of
/// large values means less catastrophic cancellation in f32.
///
/// The mean and the sum of squares are accumulated in f64. A template is at
/// most a few tens of thousands of pixels, so the CPU-side cost is negligible.
pub(crate) fn prepare_template(template: &Image<'_>, method: MatchMethod) -> PreparedTemplate {
    match method {
        MatchMethod::SumOfAbsoluteDifferences | MatchMethod::SumOfSquaredDifferences => {
            PreparedTemplate {
                data: template.data.to_vec(),
                norm: 0.0,
            }
        }
        MatchMethod::ZeroMeanDice => {
            let n = template.data.len() as f64;
            let sum: f64 = template.data.iter().map(|&v| f64::from(v)).sum();
            let mean = sum / n;

            let mut sq = 0.0f64;
            let data: Vec<f32> = template
                .data
                .iter()
                .map(|&v| {
                    let centered = f64::from(v) - mean;
                    sq += centered * centered;
                    centered as f32
                })
                .collect();

            PreparedTemplate {
                data,
                norm: sq.sqrt() as f32,
            }
        }
    }
}

/// Threshold below which a template counts as effectively uniform (a lower
/// bound on `norm`).
///
/// The only way the ZMD denominator `dev2 + tn2` can be zero is tn2 = 0, i.e. a
/// uniform template. The layer above (the load-time check in `Pattern`) rejects
/// those first, but as a defence for callers using the matcher directly we skip
/// the dispatch and return an all-zero map (a uniform template matches nothing).
pub(crate) const FLAT_TEMPLATE_EPS: f32 = 1e-6;

/// Lower bound on a template's per-pixel standard deviation for it to count as
/// having any structure.
///
/// One 8-bit step is 1/255. Below that, quantisation noise dominates and the
/// template is meaningless as something to search for, so `Pattern` raises an
/// error at load time.
///
/// Note this is **not** a runtime threshold in the matcher. ZMD has no runtime
/// guards at all (its denominator is structurally positive). This is a
/// load-time usability check, decided deterministically in f64 on the CPU.
pub const MIN_TEMPLATE_STDDEV: f64 = 1.0 / 255.0;

/// Return the input with its global mean subtracted.
///
/// ZMD only ever uses quantities with the per-window mean removed, so it is
/// strictly invariant to adding a constant to the input; this does not change
/// any result. The point is purely numerical conditioning: the denominator
/// inside the shader is `sum(I^2) - sum(I)^2 / N`, a subtraction of two large
/// numbers, and without pulling the values towards zero the cancellation is
/// noticeable in f32.
///
/// At 4K this walks 8 million pixels twice (sum, then subtract). Single
/// threaded that is not negligible, so it is parallelised.
pub(crate) fn center_input(input: &Image<'_>) -> Vec<f32> {
    use rayon::prelude::*;

    let n = input.data.len() as f64;

    // Take the sum in f64. Adding 8 million values in f32 loses enough
    // precision that the mean can be off by a few percent.
    // Rayon's fold may associate differently from run to run, but at f64
    // precision the difference does not reach the score.
    let sum: f64 = input.data.par_iter().map(|&v| f64::from(v)).sum();
    let mean = (sum / n) as f32;

    input.data.par_iter().map(|&v| v - mean).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zmd_template_is_zero_mean() {
        let t = Image::new(vec![1.0, 2.0, 3.0, 4.0], 2, 2);
        let p = prepare_template(&t, MatchMethod::ZeroMeanDice);
        let sum: f32 = p.data.iter().sum();
        assert!(sum.abs() < 1e-6, "not zero-mean: {sum}");
        // sum((v - 2.5)^2) = 1.5^2 * 2 + 0.5^2 * 2 = 5.0
        assert!((p.norm - 5.0f32.sqrt()).abs() < 1e-6, "norm = {}", p.norm);
    }

    #[test]
    fn flat_template_has_zero_norm() {
        let t = Image::new(vec![0.3; 16], 4, 4);
        let p = prepare_template(&t, MatchMethod::ZeroMeanDice);
        assert!(p.norm < FLAT_TEMPLATE_EPS);
    }

    #[test]
    fn sad_template_is_passed_through() {
        let t = Image::new(vec![1.0, 2.0, 3.0, 4.0], 2, 2);
        let p = prepare_template(&t, MatchMethod::SumOfAbsoluteDifferences);
        assert_eq!(p.data, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn centering_input_is_mean_free() {
        let img = Image::new(vec![10.0, 20.0, 30.0, 40.0], 2, 2);
        let c = center_input(&img);
        let sum: f32 = c.iter().sum();
        assert!(sum.abs() < 1e-4, "sum = {sum}");
    }
}

//! Experimental production-oriented CPU implementation of ZMD.
//!
//! The scalar f64 implementation in [`crate::cpu`] remains the reference
//! oracle. This module changes only how the same terms are obtained:
//!
//! - window sum and squared sum come from f64 integral images;
//! - small templates use a direct dot product;
//! - larger templates use block FFT cross-correlation.
//!
//! Keeping this separate makes the performance migration reversible until the
//! golden, GPU-compatibility and benchmark gates all pass.

use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::prepare::{FLAT_TEMPLATE_EPS, center_input, prepare_template};
use crate::{Image, MatchMethod, result_size};

/// Below this area the existing scalar kernel is faster than building integral
/// images. Keeping that path also makes the small-template migration risk zero.
const DIRECT_MAX_AREA: usize = 24 * 24;

/// Bound peak scratch memory when a large desktop is split into FFT blocks.
const MAX_FFT_DIM: usize = 2048;

/// Reusable plans for the FFT shapes selected by the block planner.
pub struct FastCpuMatcher {
    plans: HashMap<(usize, usize), Arc<Fft2Plan>>,
}

impl Default for FastCpuMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FastCpuMatcher {
    pub fn new() -> Self {
        Self {
            plans: HashMap::new(),
        }
    }

    /// Match one template with the same ZMD score semantics as the GPU path.
    pub fn match_template(
        &mut self,
        input: &Image<'_>,
        template: &Image<'_>,
    ) -> Option<Image<'static>> {
        let (result_width, result_height) = result_size(
            (input.width, input.height),
            (template.width, template.height),
        )?;

        if template.data.len() <= DIRECT_MAX_AREA {
            return crate::cpu::match_template(input, template, MatchMethod::ZeroMeanDice);
        }

        let prepared = prepare_template(template, MatchMethod::ZeroMeanDice);
        if prepared.norm < FLAT_TEMPLATE_EPS {
            return Some(Image::new(
                vec![0.0; result_width as usize * result_height as usize],
                result_width,
                result_height,
            ));
        }

        // This is the same conditioning used by the GPU. Subtracting one
        // global constant changes neither per-window variance nor correlation
        // with a zero-mean template, but reduces f32 cancellation.
        let centered = center_input(input);
        let integral = Integrals::new(&centered, input.width as usize, input.height as usize);
        let tn2 = f64::from(prepared.norm) * f64::from(prepared.norm);
        let area = template.data.len() as f64;

        let data = self.fft_scores(
            &centered,
            input.width as usize,
            input.height as usize,
            &prepared.data,
            template.width as usize,
            template.height as usize,
            result_width as usize,
            result_height as usize,
            &integral,
            tn2,
            area,
        );

        Some(Image::new(data, result_width, result_height))
    }

    #[allow(clippy::too_many_arguments)]
    fn fft_scores(
        &mut self,
        input: &[f32],
        input_width: usize,
        input_height: usize,
        template: &[f32],
        template_width: usize,
        template_height: usize,
        result_width: usize,
        result_height: usize,
        integral: &Integrals,
        tn2: f64,
        area: f64,
    ) -> Vec<f32> {
        let fft_width = fft_dimension(template_width, result_width);
        let fft_height = fft_dimension(template_height, result_height);
        let block_width = fft_width - template_width + 1;
        let block_height = fft_height - template_height + 1;
        let plan = self
            .plans
            .entry((fft_width, fft_height))
            .or_insert_with(|| Arc::new(Fft2Plan::new(fft_width, fft_height)))
            .clone();

        let mut template_spectrum = vec![Complex32::default(); fft_width * fft_height];
        for y in 0..template_height {
            for x in 0..template_width {
                template_spectrum
                    [(template_height - 1 - y) * fft_width + (template_width - 1 - x)] =
                    Complex32::new(template[y * template_width + x], 0.0);
            }
        }
        plan.forward(&mut template_spectrum);

        let mut origins = Vec::new();
        for y in (0..result_height).step_by(block_height) {
            for x in (0..result_width).step_by(block_width) {
                origins.push((x, y));
            }
        }

        let blocks: Vec<Block> = origins
            .into_par_iter()
            .map(|(origin_x, origin_y)| {
                let out_width = block_width.min(result_width - origin_x);
                let out_height = block_height.min(result_height - origin_y);
                let patch_width = (out_width + template_width - 1).min(input_width - origin_x);
                let patch_height = (out_height + template_height - 1).min(input_height - origin_y);

                let mut spectrum = vec![Complex32::default(); fft_width * fft_height];
                for y in 0..patch_height {
                    let src = (origin_y + y) * input_width + origin_x;
                    let dst = y * fft_width;
                    for x in 0..patch_width {
                        spectrum[dst + x].re = input[src + x];
                    }
                }

                plan.forward(&mut spectrum);
                for (value, kernel) in spectrum.iter_mut().zip(&template_spectrum) {
                    *value *= *kernel;
                }
                plan.inverse(&mut spectrum);

                let scale = 1.0f32 / (fft_width * fft_height) as f32;
                let mut scores = Vec::with_capacity(out_width * out_height);
                for y in 0..out_height {
                    let conv_row = (y + template_height - 1) * fft_width;
                    for x in 0..out_width {
                        let numerator =
                            f64::from(spectrum[conv_row + x + template_width - 1].re * scale);
                        scores.push(zmd_score(
                            numerator,
                            integral,
                            origin_x + x,
                            origin_y + y,
                            template_width,
                            template_height,
                            tn2,
                            area,
                        ));
                    }
                }

                Block {
                    x: origin_x,
                    y: origin_y,
                    width: out_width,
                    height: out_height,
                    scores,
                }
            })
            .collect();

        let mut output = vec![0.0; result_width * result_height];
        for block in blocks {
            for y in 0..block.height {
                let src = y * block.width;
                let dst = (block.y + y) * result_width + block.x;
                output[dst..dst + block.width]
                    .copy_from_slice(&block.scores[src..src + block.width]);
            }
        }
        output
    }
}

struct Block {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    scores: Vec<f32>,
}

struct Integrals {
    sum: Vec<f64>,
    square: Vec<f64>,
    stride: usize,
}

impl Integrals {
    fn new(input: &[f32], width: usize, height: usize) -> Self {
        let stride = width + 1;
        let mut sum = vec![0.0; stride * (height + 1)];
        let mut square = vec![0.0; stride * (height + 1)];

        for y in 0..height {
            let mut row_sum = 0.0;
            let mut row_square = 0.0;
            for x in 0..width {
                let value = f64::from(input[y * width + x]);
                row_sum += value;
                row_square += value * value;
                let dst = (y + 1) * stride + x + 1;
                sum[dst] = sum[dst - stride] + row_sum;
                square[dst] = square[dst - stride] + row_square;
            }
        }

        Self {
            sum,
            square,
            stride,
        }
    }

    #[inline]
    fn window(&self, values: &[f64], x: usize, y: usize, width: usize, height: usize) -> f64 {
        let x1 = x + width;
        let y1 = y + height;
        values[y1 * self.stride + x1] + values[y * self.stride + x]
            - values[y * self.stride + x1]
            - values[y1 * self.stride + x]
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn zmd_score(
    numerator: f64,
    integral: &Integrals,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    tn2: f64,
    area: f64,
) -> f32 {
    let sum = integral.window(&integral.sum, x, y, width, height);
    let sum2 = integral.window(&integral.square, x, y, width, height);
    let dev2 = (sum2 - sum * sum / area).max(0.0);
    (2.0 * numerator / (dev2 + tn2)).clamp(-1.0, 1.0) as f32
}

fn fft_dimension(template: usize, result: usize) -> usize {
    let minimum = template.max(2).next_power_of_two();
    let desired_output = (template * 4).max(256usize.saturating_sub(template - 1));
    let desired = (desired_output.min(result) + template - 1).next_power_of_two();
    desired.max(minimum).min(MAX_FFT_DIM.max(minimum))
}

struct Fft2Plan {
    width: usize,
    height: usize,
    row_forward: Arc<dyn Fft<f32>>,
    row_inverse: Arc<dyn Fft<f32>>,
    column_forward: Arc<dyn Fft<f32>>,
    column_inverse: Arc<dyn Fft<f32>>,
}

impl Fft2Plan {
    fn new(width: usize, height: usize) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        Self {
            width,
            height,
            row_forward: planner.plan_fft_forward(width),
            row_inverse: planner.plan_fft_inverse(width),
            column_forward: planner.plan_fft_forward(height),
            column_inverse: planner.plan_fft_inverse(height),
        }
    }

    fn forward(&self, values: &mut [Complex32]) {
        self.transform(values, &self.row_forward, &self.column_forward);
    }

    fn inverse(&self, values: &mut [Complex32]) {
        self.transform(values, &self.row_inverse, &self.column_inverse);
    }

    fn transform(
        &self,
        values: &mut [Complex32],
        rows: &Arc<dyn Fft<f32>>,
        columns: &Arc<dyn Fft<f32>>,
    ) {
        for row in values.chunks_exact_mut(self.width) {
            rows.process(row);
        }

        let mut column = vec![Complex32::default(); self.height];
        for x in 0..self.width {
            for y in 0..self.height {
                column[y] = values[y * self.width + x];
            }
            columns.process(&mut column);
            for y in 0..self.height {
                values[y * self.width + x] = column[y];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_correlation_finds_an_odd_sized_template() {
        let width = 80;
        let height = 70;
        let mut scene = vec![0.2; width * height];
        let tw = 25;
        let th = 27;
        let mut template = Vec::with_capacity(tw * th);
        for y in 0..th {
            for x in 0..tw {
                template.push(((x * 17 + y * 31) % 251) as f32 / 251.0);
            }
        }
        for y in 0..th {
            let dst = (19 + y) * width + 23;
            scene[dst..dst + tw].copy_from_slice(&template[y * tw..(y + 1) * tw]);
        }

        let result = FastCpuMatcher::new()
            .match_template(
                &Image::new(scene, width as u32, height as u32),
                &Image::new(template, tw as u32, th as u32),
            )
            .unwrap();
        let peak = crate::find_extremes(&result);
        assert_eq!(peak.max_value_location, (23, 19));
        assert!((peak.max_value - 1.0).abs() < 5e-5, "{}", peak.max_value);
    }
}

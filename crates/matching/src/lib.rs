//! Mekiki's matching core.
//!
//! A fork of
//! [urholaukkarinen/template-matching](https://github.com/urholaukkarinen/template-matching)
//! (MIT), brought up to date with current wgpu and extended with ZMD
//! (zero-mean Dice similarity). See `NOTICE` at the repository root for
//! attribution and the list of changes made after the fork.
//!
//! This used to be NCC, compatible with OpenCV `TM_CCOEFF_NORMED`. For the
//! purpose of finding UI elements, contrast invariance turned out to be
//! actively harmful (blank areas would claim a "perfect match"), so it was
//! replaced with ZMD on 2026-08-15. See `docs/architecture/matching.md`.
//!
//! # Coordinate system
//!
//! Images are row-major f32 greyscale. A score map is
//! `(W - w + 1) x (H - h + 1)`, and index `(x, y)` is "the score when the
//! top-left of the template is placed at `(x, y)` in the input".
//!
//! # Example
//!
//! ```ignore
//! use mekiki_matching::{Image, MatchMethod, TemplateMatcher, find_matches, NmsOptions};
//!
//! let mut matcher = TemplateMatcher::new()?;
//! let scores = matcher.match_template(&scene, &needle, MatchMethod::ZeroMeanDice)?;
//! let hits = find_matches(&scores, (needle.width, needle.height), &NmsOptions::similar(0.7));
//! ```

#![forbid(unsafe_code)]

pub mod cpu;
pub mod cpu_fast;
mod gpu;
pub mod nms;
mod prepare;

pub use gpu::{MatchError, TemplateMatcher, Tiling};
pub use prepare::MIN_TEMPLATE_STDDEV;

/// Whether the tiled variant is usable at this template size.
///
/// Exists so tests and benchmarks can report whether tiling actually happened.
pub fn tile_fits_for_test(template: (u32, u32)) -> bool {
    gpu::tile_fits(template)
}
pub use nms::{Match, NmsOptions, find_matches};

use std::borrow::Cow;

/// Matching method.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MatchMethod {
    /// Sum of absolute differences. Lower is a better match. Fragile under
    /// brightness and contrast changes.
    SumOfAbsoluteDifferences,
    /// Sum of squared differences (equivalent to OpenCV `TM_SQDIFF`).
    /// Lower is a better match.
    SumOfSquaredDifferences,
    /// ZMD (zero-mean Dice similarity). Higher is a better match, range
    /// `-1.0..=1.0`.
    ///
    /// ```text
    /// s = 2·Σ(I'·T') / (Σ I'² + Σ T'²)   I' = I - mean(window),  T' = T - mean(T)
    /// ```
    ///
    /// Its relation to NCC (OpenCV `TM_CCOEFF_NORMED`) is `s = 2rg/(1+g²)`,
    /// where r is NCC and g is the contrast ratio between window and template.
    /// At equal contrast (g = 1) it is exactly NCC, and a contrast mismatch is
    /// penalised symmetrically in g and 1/g.
    ///
    /// Invariant to a brightness **offset** (both sides have their mean
    /// removed). **Not** invariant to a contrast **gain** — that is the point.
    /// Because NCC is contrast invariant, it returned a score of 1.0 for a copy
    /// of the template flattened below the quantisation step (visually blank),
    /// which caused false positives in a real application. The ZMD denominator
    /// is held up from below by `Σ T'² > 0`, so no 0/0 exists structurally and
    /// neither clamping nor a uniform-window guard is needed. See
    /// `docs/architecture/matching.md`.
    ZeroMeanDice,
}

impl MatchMethod {
    /// The corresponding WGSL entry point name.
    pub(crate) fn entry_point(self) -> &'static str {
        match self {
            Self::SumOfAbsoluteDifferences => "main_sad",
            Self::SumOfSquaredDifferences => "main_ssd",
            Self::ZeroMeanDice => "main_zmd",
        }
    }

    /// `true` if a higher score means a better match.
    pub fn higher_is_better(self) -> bool {
        matches!(self, Self::ZeroMeanDice)
    }
}

/// An f32 greyscale image (row-major, borrowed or owned).
#[derive(Clone, Debug)]
pub struct Image<'a> {
    pub data: Cow<'a, [f32]>,
    pub width: u32,
    pub height: u32,
}

impl<'a> Image<'a> {
    /// # Panics
    /// Panics when `data.len() != width * height`.
    pub fn new(data: impl Into<Cow<'a, [f32]>>, width: u32, height: u32) -> Self {
        let data = data.into();
        assert_eq!(
            data.len(),
            (width as usize) * (height as usize),
            "pixel count does not match width * height"
        );
        Self {
            data,
            width,
            height,
        }
    }

    /// Build a `0.0..=1.0` f32 image from 8-bit greyscale.
    ///
    /// ZMD is invariant as long as input and template go through the **same**
    /// linear transform, so if both pass through this function the `/255.0`
    /// does not affect the score. Note that scaling only one of them shows up
    /// as a contrast mismatch and is penalised. For SAD/SSD it always matters,
    /// so keep it consistent with whatever you are comparing against.
    pub fn from_luma8(bytes: &[u8], width: u32, height: u32) -> Image<'static> {
        let data: Vec<f32> = bytes.iter().map(|&b| f32::from(b) / 255.0).collect();
        Image::new(data, width, height)
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> f32 {
        self.data[(y as usize) * (self.width as usize) + (x as usize)]
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Convert to an owned image.
    pub fn into_owned(self) -> Image<'static> {
        Image {
            data: Cow::Owned(self.data.into_owned()),
            width: self.width,
            height: self.height,
        }
    }
}

/// The minimum and maximum in a score map, with their locations.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Extremes {
    pub min_value: f32,
    pub max_value: f32,
    pub min_value_location: (u32, u32),
    pub max_value_location: (u32, u32),
}

/// Find the minimum and maximum of a score map and where they are.
///
/// On ties, the first in scan order (top to bottom, left to right) wins — the
/// same rule as OpenCV `minMaxLoc`.
///
/// # Panics
/// Panics when called on an empty image.
pub fn find_extremes(input: &Image<'_>) -> Extremes {
    assert!(!input.is_empty(), "empty score map");

    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    let mut min_value_location = (0, 0);
    let mut max_value_location = (0, 0);

    for y in 0..input.height {
        let row = (y as usize) * (input.width as usize);
        for x in 0..input.width {
            let value = input.data[row + x as usize];
            if value < min_value {
                min_value = value;
                min_value_location = (x, y);
            }
            if value > max_value {
                max_value = value;
                max_value_location = (x, y);
            }
        }
    }

    Extremes {
        min_value,
        max_value,
        min_value_location,
        max_value_location,
    }
}

/// Compute the score map size from the input and template sizes.
///
/// `None` if the template is larger than the input.
pub fn result_size(input: (u32, u32), template: (u32, u32)) -> Option<(u32, u32)> {
    if template.0 == 0 || template.1 == 0 || template.0 > input.0 || template.1 > input.1 {
        return None;
    }
    Some((input.0 - template.0 + 1, input.1 - template.1 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_size_rules() {
        assert_eq!(result_size((100, 50), (10, 5)), Some((91, 46)));
        assert_eq!(result_size((100, 50), (100, 50)), Some((1, 1)));
        assert_eq!(result_size((100, 50), (101, 50)), None);
        assert_eq!(result_size((100, 50), (0, 5)), None);
    }

    #[test]
    fn extremes_pick_first_in_scan_order() {
        let img = Image::new(vec![1.0, 5.0, 5.0, 1.0], 2, 2);
        let e = find_extremes(&img);
        assert_eq!(e.max_value_location, (1, 0));
        assert_eq!(e.min_value_location, (0, 0));
    }

    #[test]
    fn higher_is_better_only_for_zmd() {
        assert!(MatchMethod::ZeroMeanDice.higher_is_better());
        assert!(!MatchMethod::SumOfSquaredDifferences.higher_is_better());
    }
}

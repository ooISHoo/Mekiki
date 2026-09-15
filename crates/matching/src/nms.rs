//! Non-maximum suppression (NMS). This is what turns a score map into the
//! multiple matches that SikuliX's `findAll` returns.
//!
//! On a score map, high scores cluster around each target, so returning every
//! point above the threshold gives dozens of duplicates of the same thing.
//! Here we apply greedy suppression in score order and keep only
//! non-overlapping representatives.

use crate::{Image, MatchMethod};

/// One extracted match. The rectangle is exactly the template size.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Match {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub score: f32,
}

impl Match {
    /// Centre of the rectangle. Used as the click point.
    pub fn center(&self) -> (u32, u32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    fn area(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// IoU (intersection over union) with another match.
    fn iou(&self, other: &Match) -> f32 {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = (self.x + self.width).min(other.x + other.width);
        let y2 = (self.y + self.height).min(other.y + other.height);

        if x2 <= x1 || y2 <= y1 {
            return 0.0;
        }

        let inter = u64::from(x2 - x1) * u64::from(y2 - y1);
        let union = self.area() + other.area() - inter;
        (inter as f64 / union as f64) as f32
    }
}

/// NMS settings.
#[derive(Copy, Clone, Debug)]
pub struct NmsOptions {
    /// Score threshold for acceptance. "At least this" when
    /// `higher_is_better`, "at most this" otherwise.
    pub threshold: f32,
    /// Discard a candidate whose IoU with an already-kept match exceeds this.
    ///
    /// The default of 0.0 means "discard on even a single pixel of overlap",
    /// matching SikuliX's findAll behaviour of returning disjoint regions.
    pub max_overlap: f32,
    /// Maximum number of matches to return.
    pub max_results: usize,
    /// Whether a higher score is better. Pass
    /// [`MatchMethod::higher_is_better`].
    pub higher_is_better: bool,
    /// Upper bound on candidate points retained before suppression runs.
    ///
    /// Suppression is O(n * kept), so a loose threshold at 4K produces millions
    /// of points and never finishes in reasonable time. We cut down to the top
    /// candidates first and suppress after that.
    pub candidate_limit: usize,
}

impl Default for NmsOptions {
    fn default() -> Self {
        Self {
            threshold: 0.7,
            max_overlap: 0.0,
            max_results: 100,
            higher_is_better: true,
            candidate_limit: 100_000,
        }
    }
}

impl NmsOptions {
    /// For ZMD. Corresponds to SikuliX's `Pattern.similar(threshold)`.
    pub fn similar(threshold: f32) -> Self {
        Self {
            threshold,
            higher_is_better: true,
            ..Default::default()
        }
    }

    /// Defaults appropriate to the method. SAD/SSD become "lower is better".
    pub fn for_method(method: MatchMethod, threshold: f32) -> Self {
        Self {
            threshold,
            higher_is_better: method.higher_is_better(),
            ..Default::default()
        }
    }

    pub fn with_max_results(mut self, n: usize) -> Self {
        self.max_results = n;
        self
    }

    pub fn with_max_overlap(mut self, overlap: f32) -> Self {
        self.max_overlap = overlap;
        self
    }

    fn passes(&self, score: f32) -> bool {
        if self.higher_is_better {
            score >= self.threshold
        } else {
            score <= self.threshold
        }
    }
}

/// Extract a set of non-overlapping matches from a score map.
///
/// The result is ordered best-first (descending for ZMD).
pub fn find_matches(
    scores: &Image<'_>,
    template_size: (u32, u32),
    options: &NmsOptions,
) -> Vec<Match> {
    let (tw, th) = template_size;
    if tw == 0 || th == 0 || options.max_results == 0 {
        return Vec::new();
    }

    let mut candidates: Vec<Match> = Vec::new();
    for y in 0..scores.height {
        let row = (y as usize) * (scores.width as usize);
        for x in 0..scores.width {
            let score = scores.data[row + x as usize];
            if score.is_nan() || !options.passes(score) {
                continue;
            }
            candidates.push(Match {
                x,
                y,
                width: tw,
                height: th,
                score,
            });
        }
    }

    if candidates.is_empty() {
        return Vec::new();
    }

    // Sort best-first. NaN was filtered out above, so no total order is needed.
    let better = |a: &Match, b: &Match| {
        if options.higher_is_better {
            b.score.partial_cmp(&a.score).unwrap()
        } else {
            a.score.partial_cmp(&b.score).unwrap()
        }
    };

    // With too many candidates, keep only the top ones before suppressing.
    if candidates.len() > options.candidate_limit {
        log::debug!(
            "narrowing {} NMS candidates down to the top {}",
            candidates.len(),
            options.candidate_limit
        );
        candidates.select_nth_unstable_by(options.candidate_limit, better);
        candidates.truncate(options.candidate_limit);
    }

    candidates.sort_by(better);

    let mut kept: Vec<Match> = Vec::new();
    for cand in candidates {
        if kept.len() >= options.max_results {
            break;
        }
        if kept.iter().any(|k| k.iou(&cand) > options.max_overlap) {
            continue;
        }
        kept.push(cand);
    }

    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A score map with three peaks, each surrounded by high scores as well.
    fn peaky_map() -> Image<'static> {
        let (w, h) = (40u32, 40u32);
        let mut data = vec![0.0f32; (w * h) as usize];
        for &(px, py, peak) in &[(5u32, 5u32, 0.99f32), (25, 8, 0.90), (10, 30, 0.75)] {
            for dy in 0..3u32 {
                for dx in 0..3u32 {
                    let idx = ((py + dy) * w + px + dx) as usize;
                    data[idx] = peak - 0.01 * (dx + dy) as f32;
                }
            }
        }
        Image::new(data, w, h)
    }

    #[test]
    fn suppresses_neighbours_and_orders_by_score() {
        let map = peaky_map();
        let hits = find_matches(&map, (8, 8), &NmsOptions::similar(0.7));
        assert_eq!(hits.len(), 3, "{hits:?}");
        assert_eq!((hits[0].x, hits[0].y), (5, 5));
        assert_eq!((hits[1].x, hits[1].y), (25, 8));
        assert_eq!((hits[2].x, hits[2].y), (10, 30));
        assert!(hits[0].score > hits[1].score && hits[1].score > hits[2].score);
    }

    #[test]
    fn threshold_filters_weak_peaks() {
        let map = peaky_map();
        let hits = find_matches(&map, (8, 8), &NmsOptions::similar(0.8));
        assert_eq!(hits.len(), 2, "{hits:?}");
    }

    #[test]
    fn max_results_caps_output() {
        let map = peaky_map();
        let hits = find_matches(&map, (8, 8), &NmsOptions::similar(0.7).with_max_results(1));
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].x, hits[0].y), (5, 5));
    }

    #[test]
    fn lower_is_better_for_ssd() {
        // SSD-like: smaller is better. Arrange for exactly one 0.0 point to pass.
        let mut data = vec![10.0f32; 16];
        data[5] = 0.0;
        let map = Image::new(data, 4, 4);
        let opts = NmsOptions::for_method(MatchMethod::SumOfSquaredDifferences, 1.0);
        let hits = find_matches(&map, (2, 2), &opts);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].x, hits[0].y), (1, 1));
    }

    #[test]
    fn overlap_allowance_keeps_adjacent_hits() {
        let map = peaky_map();
        // Allowing IoU up to 0.9 keeps the points around each peak as separate
        // matches.
        let hits = find_matches(
            &map,
            (8, 8),
            &NmsOptions::similar(0.7).with_max_overlap(0.9),
        );
        assert!(
            hits.len() > 3,
            "the overlap allowance had no effect: {}",
            hits.len()
        );
    }

    #[test]
    fn iou_of_disjoint_rects_is_zero() {
        let a = Match {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
            score: 1.0,
        };
        let b = Match {
            x: 20,
            y: 20,
            width: 10,
            height: 10,
            score: 1.0,
        };
        assert_eq!(a.iou(&b), 0.0);
    }

    #[test]
    fn iou_of_identical_rects_is_one() {
        let a = Match {
            x: 3,
            y: 4,
            width: 10,
            height: 10,
            score: 1.0,
        };
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn center_is_rect_midpoint() {
        let a = Match {
            x: 10,
            y: 20,
            width: 8,
            height: 6,
            score: 1.0,
        };
        assert_eq!(a.center(), (14, 23));
    }
}

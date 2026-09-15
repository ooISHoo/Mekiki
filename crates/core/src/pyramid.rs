//! The multi-resolution pyramid, and the coarse-search-then-refine loop.
//!
//! Phase 1-3 of the development plan. This is the main answer to the problem
//! Phase 0 uncovered: past about 100px of template, a naive exhaustive search
//! loses to OpenCV's DFT.
//!
//! Scaling a 128px template down by 4 makes it equivalent to 32px, which lands
//! in the range where Phase 0 measured the GPU 3.8x faster. Refinement only
//! looks at a small window around each candidate, so its cost is close to
//! negligible.

use mekiki_matching::Image;

/// Halve the image with a 2x2 box average.
///
/// A box rather than a Gaussian because, as long as needle and haystack get the
/// **same** reduction, preserving the score semantics is all that is needed.
/// Odd sizes are truncated (the last column and row are dropped).
pub fn downsample_half(src: &Image<'_>) -> Image<'static> {
    let w = (src.width / 2).max(1);
    let h = (src.height / 2).max(1);
    let sw = src.width as usize;

    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        let r0 = (y * 2) * sw;
        let r1 = r0 + sw;
        for x in 0..w as usize {
            let c0 = x * 2;
            let c1 = c0 + 1;
            let v = src.data[r0 + c0] + src.data[r0 + c1] + src.data[r1 + c0] + src.data[r1 + c1];
            data.push(v * 0.25);
        }
    }
    Image::new(data, w, h)
}

/// Decide how many pyramid levels to build.
///
/// `levels[0]` is full size, and the count goes as far as keeping the needle's
/// shorter side at or above `min_size`. Too small a needle makes matching
/// unstable, so it stops there.
pub fn level_count(needle: (u32, u32), max_levels: u32, min_size: u32) -> u32 {
    let mut levels = 0;
    let mut w = needle.0;
    let mut h = needle.1;
    while levels < max_levels && w / 2 >= min_size && h / 2 >= min_size {
        w /= 2;
        h /= 2;
        levels += 1;
    }
    levels
}

/// Build a sequence reduced `count` times, with `levels[0]` at full size.
pub fn build(source: &Image<'_>, count: u32) -> Vec<Image<'static>> {
    let mut levels = Vec::with_capacity(count as usize + 1);
    levels.push(source.clone().into_owned());
    for i in 0..count as usize {
        levels.push(downsample_half(&levels[i]));
    }
    levels
}

/// Crop a rectangle out of an image, clamping anything out of range.
///
/// Returns the cropped image and where its top-left sits in the original.
pub fn crop_clamped(
    src: &Image<'_>,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Option<(Image<'static>, u32, u32)> {
    if w == 0 || h == 0 || src.width < w || src.height < h {
        return None;
    }
    // Clamp the position while keeping the size. Changing the size would force
    // the matcher to recreate its buffers, meaning a GPU buffer swap per
    // candidate.
    let x0 = x.clamp(0, (src.width - w) as i32) as u32;
    let y0 = y.clamp(0, (src.height - h) as i32) as u32;

    let sw = src.width as usize;
    let mut data = Vec::with_capacity((w * h) as usize);
    for row in 0..h as usize {
        let start = (y0 as usize + row) * sw + x0 as usize;
        data.extend_from_slice(&src.data[start..start + w as usize]);
    }
    Some((Image::new(data, w, h), x0, y0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_averages_2x2_blocks() {
        let src = Image::new(vec![0.0, 1.0, 2.0, 3.0], 2, 2);
        let out = downsample_half(&src);
        assert_eq!((out.width, out.height), (1, 1));
        assert!((out.data[0] - 1.5).abs() < 1e-6);
    }

    #[test]
    fn downsample_drops_odd_edges() {
        let src = Image::new(vec![1.0; 15], 5, 3);
        let out = downsample_half(&src);
        assert_eq!((out.width, out.height), (2, 1));
    }

    #[test]
    fn level_count_stops_at_min_size() {
        // 128 -> 64 -> 32 -> 16; with min 12 that is 3 levels.
        assert_eq!(level_count((128, 128), 5, 12), 3);
        // Capped by the maximum.
        assert_eq!(level_count((128, 128), 2, 12), 2);
        // Already small: 0 levels.
        assert_eq!(level_count((16, 16), 5, 12), 0);
        // Decided by the shorter side.
        assert_eq!(level_count((128, 20), 5, 12), 0);
        assert_eq!(level_count((128, 48), 5, 12), 2);
    }

    #[test]
    fn build_produces_count_plus_one_levels() {
        let src = Image::new(vec![0.5; 64 * 64], 64, 64);
        let levels = build(&src, 2);
        assert_eq!(levels.len(), 3);
        assert_eq!((levels[0].width, levels[0].height), (64, 64));
        assert_eq!((levels[1].width, levels[1].height), (32, 32));
        assert_eq!((levels[2].width, levels[2].height), (16, 16));
    }

    #[test]
    fn crop_keeps_size_and_clamps_position() {
        let src = Image::new((0..100).map(|v| v as f32).collect::<Vec<_>>(), 10, 10);
        // Inside the bounds.
        let (c, x, y) = crop_clamped(&src, 3, 4, 2, 2).unwrap();
        assert_eq!((x, y), (3, 4));
        assert_eq!(c.data, vec![43.0, 44.0, 53.0, 54.0]);

        // Overflowing bottom-right: the size is kept and the position clamped.
        let (c, x, y) = crop_clamped(&src, 9, 9, 3, 3).unwrap();
        assert_eq!((c.width, c.height), (3, 3));
        assert_eq!((x, y), (7, 7));

        // Negative positions are clamped too.
        let (_, x, y) = crop_clamped(&src, -5, -5, 3, 3).unwrap();
        assert_eq!((x, y), (0, 0));

        // The window is larger than the source image.
        assert!(crop_clamped(&src, 0, 0, 20, 20).is_none());
    }
}

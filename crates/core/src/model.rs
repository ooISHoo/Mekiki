//! The object model: Region / Pattern / Match.
//!
//! The shape takes SikuliX as a reference (a design with a track record in
//! image-based RPA), but compatibility is not a goal. See §1 of the development
//! plan for the details.
//!
//! - `Pattern::similar(x)` is a threshold on the ZMD score (0.7 by default)
//! - a `Match` is clicked at the centre of its rectangle, offset by
//!   `target_offset` when present
//! - coordinates are physical pixels in virtual desktop space

use std::sync::Arc;

use mekiki_capture::Rect;
use mekiki_matching::Image;

use crate::pyramid;

/// A region to search within.
///
/// Corresponds to SikuliX's `Region`. A `Screen` is nothing more than "a Region
/// covering a whole display", so it does not get its own type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub rect: Rect,
    /// The index of the display this region belongs to.
    pub display: usize,
    /// Native top-level window which owns this region, when it came from a
    /// `window:` locator. UI Automation uses it to stay inside that window's
    /// subtree instead of scanning every overlapping application.
    pub window: Option<isize>,
}

impl Region {
    pub fn new(rect: Rect, display: usize) -> Self {
        Self {
            rect,
            display,
            window: None,
        }
    }

    pub fn with_window(mut self, hwnd: isize) -> Self {
        self.window = Some(hwnd);
        self
    }

    fn derived(&self, rect: Rect) -> Self {
        Self {
            rect,
            display: self.display,
            window: self.window,
        }
    }

    pub fn center(&self) -> (i32, i32) {
        (
            self.rect.x + self.rect.width as i32 / 2,
            self.rect.y + self.rect.height as i32 / 2,
        )
    }

    /// Grow by the given amount (shrink when negative). Does not clip at
    /// display boundaries.
    pub fn grow(&self, by: i32) -> Region {
        let w = (self.rect.width as i32 + by * 2).max(0) as u32;
        let h = (self.rect.height as i32 + by * 2).max(0) as u32;
        self.derived(Rect::new(self.rect.x - by, self.rect.y - by, w, h))
    }

    /// The equivalent of SikuliX's `Region.offset`.
    pub fn offset(&self, dx: i32, dy: i32) -> Region {
        self.derived(Rect::new(
            self.rect.x + dx,
            self.rect.y + dy,
            self.rect.width,
            self.rect.height,
        ))
    }

    /// SikuliX's region arithmetic for taking the area above, below and so on.
    pub fn above(&self, height: u32) -> Region {
        self.derived(Rect::new(
            self.rect.x,
            self.rect.y - height as i32,
            self.rect.width,
            height,
        ))
    }

    pub fn below(&self, height: u32) -> Region {
        self.derived(Rect::new(
            self.rect.x,
            self.rect.bottom(),
            self.rect.width,
            height,
        ))
    }

    pub fn left(&self, width: u32) -> Region {
        self.derived(Rect::new(
            self.rect.x - width as i32,
            self.rect.y,
            width,
            self.rect.height,
        ))
    }

    pub fn right(&self, width: u32) -> Region {
        self.derived(Rect::new(
            self.rect.right(),
            self.rect.y,
            width,
            self.rect.height,
        ))
    }
}

/// The pattern being searched for (the needle).
///
/// It builds and holds a multi-resolution pyramid at construction. This is the
/// plan's Phase 1-3 item, "pre-build and cache the needle image's
/// multi-resolution pyramid". As long as a `Pattern` is reused, the pyramid is
/// built exactly once.
#[derive(Clone)]
pub struct Pattern {
    /// `levels[0]` is full size; each level after that is half again.
    levels: Arc<Vec<Image<'static>>>,
    similar: f32,
    target_offset: (i32, i32),
    name: String,
}

impl std::fmt::Debug for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pattern")
            .field("name", &self.name)
            .field("size", &(self.width(), self.height()))
            .field("similar", &self.similar)
            .field("levels", &self.levels.len())
            .finish()
    }
}

impl Pattern {
    /// Build a pattern from a greyscale image.
    ///
    /// `max_levels` / `min_level_size` decide how many pyramid levels there
    /// are. Normally the values from [`crate::Settings`] are used.
    pub fn from_image(
        image: Image<'_>,
        name: impl Into<String>,
        similar: f32,
        max_levels: u32,
        min_level_size: u32,
    ) -> Self {
        let count = pyramid::level_count((image.width, image.height), max_levels, min_level_size);
        Self {
            levels: Arc::new(pyramid::build(&image, count)),
            similar,
            target_offset: (0, 0),
            name: name.into(),
        }
    }

    /// Return a copy with a different threshold. The equivalent of SikuliX's
    /// `Pattern.similar(0.9)`.
    ///
    /// The pyramid is shared through an `Arc`, so it is not rebuilt.
    pub fn similar(&self, similar: f32) -> Self {
        Self {
            similar: similar.clamp(0.0, 1.0),
            ..self.clone()
        }
    }

    /// Shift the click point away from the centre of the rectangle. The
    /// equivalent of SikuliX's `Pattern.targetOffset`.
    pub fn target_offset(&self, dx: i32, dy: i32) -> Self {
        Self {
            target_offset: (dx, dy),
            ..self.clone()
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn similarity(&self) -> f32 {
        self.similar
    }

    pub fn offset(&self) -> (i32, i32) {
        self.target_offset
    }

    pub fn width(&self) -> u32 {
        self.levels[0].width
    }

    pub fn height(&self) -> u32 {
        self.levels[0].height
    }

    /// The number of pyramid levels, not counting full size.
    pub fn max_level(&self) -> u32 {
        self.levels.len() as u32 - 1
    }

    pub fn level(&self, level: u32) -> &Image<'static> {
        &self.levels[level as usize]
    }
}

/// The text being searched for (the text counterpart of a needle).
///
/// The same standing as the image [`Pattern`]. The `similar()` threshold
/// corresponds to the value from [`mekiki_ocr::match_score`]. It is on the same
/// scale as image matching, so scripts can write both without distinction.
#[derive(Clone, Debug)]
pub struct TextPattern {
    query: String,
    similar: f32,
    target_offset: (i32, i32),
    /// When true, a whole line counts as one candidate. When false, runs of
    /// words within a line are searched instead.
    ///
    /// False by default: a whole line gives a wide rectangle and the click
    /// point ends up nowhere useful.
    whole_line: bool,
}

impl TextPattern {
    pub fn new(query: impl Into<String>, similar: f32) -> Self {
        Self {
            query: query.into(),
            similar: similar.clamp(0.0, 1.0),
            target_offset: (0, 0),
            whole_line: false,
        }
    }

    pub fn similar(&self, similar: f32) -> Self {
        Self {
            similar: similar.clamp(0.0, 1.0),
            ..self.clone()
        }
    }

    pub fn target_offset(&self, dx: i32, dy: i32) -> Self {
        Self {
            target_offset: (dx, dy),
            ..self.clone()
        }
    }

    /// Treat a whole line as a single candidate.
    pub fn whole_line(&self, enabled: bool) -> Self {
        Self {
            whole_line: enabled,
            ..self.clone()
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn similarity(&self) -> f32 {
        self.similar
    }

    pub fn offset(&self) -> (i32, i32) {
        self.target_offset
    }

    pub fn is_whole_line(&self) -> bool {
        self.whole_line
    }
}

/// The UI element being searched for (the accessibility counterpart of a needle).
///
/// The same standing as [`Pattern`] / [`TextPattern`]. The `similar()` threshold
/// corresponds to how well the name matches, and [`mekiki_ocr::match_score`] is
/// reused to decide it.
///
/// **Sharing the matcher with OCR is deliberate.** From the author's point of
/// view, "search for `Save`" should mean the same thing under `ocr:` and `ui:`.
/// A UIA name is not necessarily the displayed label (in practice you see names
/// like "AppName - 1 running window, pinned"), and looking only for an exact
/// match would reject the obvious way to write it.
#[derive(Clone, Debug, Default)]
pub struct UiPattern {
    /// The display name. Omitting it means the name is not used to filter.
    name: Option<String>,
    /// The identifier the application assigned. Exact match.
    automation_id: Option<String>,
    control_type: Option<mekiki_uia::ControlType>,
    similar: f32,
    target_offset: (i32, i32),
    /// Whether disabled elements are eligible.
    ///
    /// Excluded by default. Clicking a button that cannot be pressed does
    /// nothing, which turns into the opaque failure of "it was clicked but
    /// nothing happened".
    include_disabled: bool,
}

impl UiPattern {
    pub fn new(similar: f32) -> Self {
        Self {
            similar: similar.clamp(0.0, 1.0),
            ..Default::default()
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_automation_id(mut self, id: impl Into<String>) -> Self {
        self.automation_id = Some(id.into());
        self
    }

    pub fn with_control_type(mut self, t: mekiki_uia::ControlType) -> Self {
        self.control_type = Some(t);
        self
    }

    pub fn include_disabled(mut self, enabled: bool) -> Self {
        self.include_disabled = enabled;
        self
    }

    pub fn similar(&self, similar: f32) -> Self {
        Self {
            similar: similar.clamp(0.0, 1.0),
            ..self.clone()
        }
    }

    pub fn target_offset(&self, dx: i32, dy: i32) -> Self {
        Self {
            target_offset: (dx, dy),
            ..self.clone()
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn automation_id(&self) -> Option<&str> {
        self.automation_id.as_deref()
    }

    pub fn control_type(&self) -> Option<mekiki_uia::ControlType> {
        self.control_type
    }

    pub fn similarity(&self) -> f32 {
        self.similar
    }

    pub fn offset(&self) -> (i32, i32) {
        self.target_offset
    }

    pub fn includes_disabled(&self) -> bool {
        self.include_disabled
    }

    /// Whether it carries no condition at all. Every element would match, so
    /// the caller rejects it.
    pub fn is_unconstrained(&self) -> bool {
        self.name.is_none() && self.automation_id.is_none() && self.control_type.is_none()
    }

    /// A description for error messages.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = &self.name {
            parts.push(format!("name={n}"));
        }
        if let Some(i) = &self.automation_id {
            parts.push(format!("id={i}"));
        }
        if let Some(t) = self.control_type {
            parts.push(format!("type={t}"));
        }
        parts.join(",")
    }
}

/// One match.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Match {
    /// The rectangle in virtual desktop coordinates.
    pub rect: Rect,
    pub display: usize,
    /// The ZMD score (`0.0..=1.0`).
    pub score: f32,
    /// The click offset inherited from the pattern.
    pub target_offset: (i32, i32),
}

impl Match {
    pub fn center(&self) -> (i32, i32) {
        (
            self.rect.x + self.rect.width as i32 / 2,
            self.rect.y + self.rect.height as i32 / 2,
        )
    }

    /// The coordinate to actually click: the centre plus `targetOffset`.
    pub fn target(&self) -> (i32, i32) {
        let (cx, cy) = self.center();
        (cx + self.target_offset.0, cy + self.target_offset.1)
    }

    pub fn region(&self) -> Region {
        Region::new(self.rect, self.display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: u32, h: u32) -> Pattern {
        let img = Image::new(vec![0.5f32; (w * h) as usize], w, h);
        Pattern::from_image(img, "test", 0.7, 3, 12)
    }

    #[test]
    fn pattern_builds_pyramid() {
        let p = pattern(128, 128);
        assert_eq!(p.max_level(), 3);
        assert_eq!((p.level(0).width, p.level(0).height), (128, 128));
        assert_eq!((p.level(3).width, p.level(3).height), (16, 16));
    }

    #[test]
    fn small_pattern_has_no_pyramid() {
        let p = pattern(20, 20);
        assert_eq!(p.max_level(), 0);
    }

    #[test]
    fn similar_shares_the_pyramid() {
        let p = pattern(64, 64);
        let q = p.similar(0.95);
        assert_eq!(q.similarity(), 0.95);
        assert!(Arc::ptr_eq(&p.levels, &q.levels), "the pyramid was rebuilt");
    }

    #[test]
    fn similarity_is_clamped() {
        let p = pattern(64, 64);
        assert_eq!(p.similar(1.5).similarity(), 1.0);
        assert_eq!(p.similar(-1.0).similarity(), 0.0);
    }

    #[test]
    fn match_target_applies_offset() {
        let m = Match {
            rect: Rect::new(100, 200, 20, 10),
            display: 0,
            score: 0.9,
            target_offset: (3, -4),
        };
        assert_eq!(m.center(), (110, 205));
        assert_eq!(m.target(), (113, 201));
    }

    #[test]
    fn ui_pattern_needs_at_least_one_condition() {
        assert!(UiPattern::new(0.8).is_unconstrained());
        assert!(!UiPattern::new(0.8).with_name("保存").is_unconstrained());
        assert!(
            !UiPattern::new(0.8)
                .with_control_type(mekiki_uia::ControlType::Button)
                .is_unconstrained()
        );
    }

    #[test]
    fn ui_pattern_describes_its_conditions() {
        let p = UiPattern::new(0.8)
            .with_name("保存")
            .with_control_type(mekiki_uia::ControlType::Button);
        assert_eq!(p.describe(), "name=保存,type=button");
    }

    #[test]
    fn region_directional_operations() {
        let r = Region::new(Rect::new(100, 100, 50, 40), 0);
        assert_eq!(r.center(), (125, 120));
        assert_eq!(r.below(10).rect, Rect::new(100, 140, 50, 10));
        assert_eq!(r.above(10).rect, Rect::new(100, 90, 50, 10));
        assert_eq!(r.right(10).rect, Rect::new(150, 100, 10, 40));
        assert_eq!(r.left(10).rect, Rect::new(90, 100, 10, 40));
        assert_eq!(r.grow(5).rect, Rect::new(95, 95, 60, 50));
    }
}

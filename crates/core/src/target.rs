//! The lazily evaluated [`Target`].
//!
//! The implementation of the [Rhai API execution model](../../../docs/architecture/rhai-api.md#execution-model).
//!
//! **A `Target` is a description of how to search, not a search result.**
//! Every action re-captures and re-searches, which structurally rules out the
//! accident of "the screen moved between the `find` and the `click`, so a stale
//! coordinate was pressed".

use std::time::Duration;

use mekiki_capture::Rect;

use crate::model::{Match, Pattern, Region, TextPattern, UiPattern};

/// The spatial relationship to an anchor.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Direction {
    RightOf,
    LeftOf,
    Above,
    Below,
    /// Near in any direction, by centre-to-centre distance.
    Near,
}

impl Direction {
    /// Whether a candidate satisfies this relationship to the anchor.
    ///
    /// A directional relationship also requires **overlap along the shared
    /// edge**. Saying "to the right of the label" must not sweep in something
    /// down in the bottom-right corner of the screen.
    pub(crate) fn accepts(self, anchor: Rect, candidate: Rect, max_distance: u32) -> bool {
        let max = max_distance as i64;
        match self {
            Direction::RightOf => {
                let gap = candidate.x as i64 - anchor.right() as i64;
                gap >= 0 && gap <= max && vertical_overlap(anchor, candidate)
            }
            Direction::LeftOf => {
                let gap = anchor.x as i64 - candidate.right() as i64;
                gap >= 0 && gap <= max && vertical_overlap(anchor, candidate)
            }
            Direction::Below => {
                let gap = candidate.y as i64 - anchor.bottom() as i64;
                gap >= 0 && gap <= max && horizontal_overlap(anchor, candidate)
            }
            Direction::Above => {
                let gap = anchor.y as i64 - candidate.bottom() as i64;
                gap >= 0 && gap <= max && horizontal_overlap(anchor, candidate)
            }
            Direction::Near => center_distance(anchor, candidate) <= max_distance as f64,
        }
    }
}

fn vertical_overlap(a: Rect, b: Rect) -> bool {
    a.y < b.bottom() && b.y < a.bottom()
}

fn horizontal_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.right() && b.x < a.right()
}

pub(crate) fn center_distance(a: Rect, b: Rect) -> f64 {
    let ax = a.x as f64 + a.width as f64 / 2.0;
    let ay = a.y as f64 + a.height as f64 / 2.0;
    let bx = b.x as f64 + b.width as f64 / 2.0;
    let by = b.y as f64 + b.height as f64 / 2.0;
    ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt()
}

/// The thing being searched for: an image, some text or a UI element.
///
/// [`Target`] holds this enum so scripts can treat all of them alike. That is
/// why `target("ok.png")`, `target("ocr:Save")` and `target("ui:name=Save")`
/// are all written the same way.
#[derive(Clone, Debug)]
pub enum Needle {
    Image(Pattern),
    Text(TextPattern),
    /// Search via the accessibility API (UI Automation on Windows).
    Ui(UiPattern),
}

impl Needle {
    pub fn similarity(&self) -> f32 {
        match self {
            Needle::Image(p) => p.similarity(),
            Needle::Text(t) => t.similarity(),
            Needle::Ui(u) => u.similarity(),
        }
    }

    pub fn offset(&self) -> (i32, i32) {
        match self {
            Needle::Image(p) => p.offset(),
            Needle::Text(t) => t.offset(),
            Needle::Ui(u) => u.offset(),
        }
    }

    /// The name used in error messages.
    pub fn name(&self) -> String {
        match self {
            Needle::Image(p) => p.name().to_string(),
            Needle::Text(t) => format!("ocr:{}", t.query()),
            Needle::Ui(u) => format!("ui:{}", u.describe()),
        }
    }

    /// Returns a reference for an image needle, `None` otherwise.
    pub fn as_image(&self) -> Option<&Pattern> {
        match self {
            Needle::Image(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&TextPattern> {
        match self {
            Needle::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_ui(&self) -> Option<&UiPattern> {
        match self {
            Needle::Ui(u) => Some(u),
            _ => None,
        }
    }

    fn with_similar(&self, v: f32) -> Self {
        match self {
            Needle::Image(p) => Needle::Image(p.similar(v)),
            Needle::Text(t) => Needle::Text(t.similar(v)),
            Needle::Ui(u) => Needle::Ui(u.similar(v)),
        }
    }

    fn with_offset(&self, dx: i32, dy: i32) -> Self {
        match self {
            Needle::Image(p) => Needle::Image(p.target_offset(dx, dy)),
            Needle::Text(t) => Needle::Text(t.target_offset(dx, dy)),
            Needle::Ui(u) => Needle::Ui(u.target_offset(dx, dy)),
        }
    }
}

/// An anchor-relative constraint.
///
/// The anchor itself may be an image or text. "The input field to the right of
/// the label `Name:`" reads far more naturally when the label can be given as
/// text.
#[derive(Clone, Debug)]
pub struct Relation {
    pub direction: Direction,
    pub anchor: Needle,
    pub max_distance: u32,
}

/// Which of several candidates to take.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// The best score.
    Best,
    /// The n-th in sort order (0 based).
    Nth(usize),
    First,
    Last,
}

/// How multiple matches are ordered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MatchOrder {
    /// Descending by score.
    Score,
    /// Reading order (top to bottom, left to right).
    ///
    /// When a person says "the third checkbox" on screen, this is what they
    /// mean — not score order.
    ReadingOrder,
}

/// A description of how to search.
///
/// Constructing one does nothing. Capture and search happen when it is handed
/// to [`crate::Mekiki::on`].
#[derive(Clone, Debug)]
pub struct Target {
    pub(crate) region: Region,
    pub(crate) needle: Needle,
    /// Alternative ways to search, tried in order when the primary one fails.
    ///
    /// This is the "hybrid" from Phase 4-4 of the development plan.
    /// **The author decides** which comes first. The order maps directly to
    /// performance and the right answer varies by situation, so it is better
    /// not to fix it here. See the [UIA maintenance notes](../../../docs/maintenance/windows-capture-and-uia.md) for
    /// guidance.
    pub(crate) fallbacks: Vec<Needle>,
    pub(crate) relation: Option<Relation>,
    pub(crate) selection: Selection,
    /// `None` follows [`crate::Settings::match_order`].
    pub(crate) order: Option<MatchOrder>,
    /// `None` follows [`crate::Settings::auto_wait_timeout`].
    pub(crate) timeout: Option<Duration>,
    /// When true, the stability check is skipped.
    pub(crate) force: bool,
}

impl Target {
    pub(crate) fn new(region: Region, needle: Needle) -> Self {
        Self {
            region,
            needle,
            fallbacks: Vec::new(),
            relation: None,
            selection: Selection::Best,
            order: None,
            timeout: None,
            force: false,
        }
    }

    /// Replace the search area.
    pub fn in_region(mut self, region: Region) -> Self {
        self.region = region;
        self
    }

    /// Change the similarity threshold.
    ///
    /// A ZMD score for images, a string match score for text. They are on the
    /// same scale, so it is written the same way either way.
    pub fn similar(mut self, similar: f32) -> Self {
        self.needle = self.needle.with_similar(similar);
        self
    }

    /// Shift the click point away from the centre of the rectangle.
    pub fn offset(mut self, dx: i32, dy: i32) -> Self {
        self.needle = self.needle.with_offset(dx, dy);
        self
    }

    /// Change the wait time for this target alone.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Drop the stability check.
    ///
    /// An escape hatch for targets that never settle, such as something that
    /// animates continuously. The risk of grabbing a half-drawn frame comes
    /// back, so use it only where it is needed.
    pub fn force(mut self) -> Self {
        self.force = true;
        self
    }

    /// State the ordering explicitly.
    pub fn order(mut self, order: MatchOrder) -> Self {
        self.order = Some(order);
        self
    }

    /// The n-th in sort order (0 based).
    pub fn nth(mut self, n: usize) -> Self {
        self.selection = Selection::Nth(n);
        self
    }

    pub fn first(mut self) -> Self {
        self.selection = Selection::First;
        self
    }

    pub fn last(mut self) -> Self {
        self.selection = Selection::Last;
        self
    }

    /// Take the one with the best score.
    pub fn best(mut self) -> Self {
        self.selection = Selection::Best;
        self
    }

    /// Add an alternative way to search, tried when the primary one fails.
    ///
    /// Calling it repeatedly chains any number of them. They are tried in the
    /// order written.
    ///
    /// ```ignore
    /// // Search by image, and fall back to the accessibility API on a miss.
    /// // Keeps working when a theme or DPI change stops the template matching.
    /// target("save.png").or(Needle::Ui(UiPattern::new(0.8).with_name("Save")))
    /// ```
    ///
    /// The search area, ordering, selection and wait time all come from the
    /// primary. **Only the way of searching** changes.
    pub fn or(mut self, alternative: Needle) -> Self {
        self.fallbacks.push(alternative);
        self
    }

    /// The primary followed by the alternatives, in order.
    pub(crate) fn needles(&self) -> impl Iterator<Item = &Needle> {
        std::iter::once(&self.needle).chain(self.fallbacks.iter())
    }

    /// Whether this can be resolved **without capturing the screen**.
    ///
    /// True only when every way of searching — the primary, every alternative,
    /// and the anchor — goes through the accessibility API, which reads the UI
    /// tree rather than pixels.
    ///
    /// One image or `ocr:` fallback makes the whole target need a frame, so this
    /// deliberately demands all of them. A target that is `ui:` *or* an image is
    /// asking for the image path to be available.
    ///
    /// This is what lets `ui:` keep working where capture cannot: a display that
    /// refuses duplication, protected content, a session without a desktop.
    pub fn is_ui_only(&self) -> bool {
        let anchor_is_ui = match &self.relation {
            None => true,
            Some(relation) => matches!(relation.anchor, Needle::Ui(_)),
        };
        anchor_is_ui && self.needles().all(|n| matches!(n, Needle::Ui(_)))
    }

    /// Restrict to things on the right of the anchor.
    pub fn right_of(self, anchor: &Pattern, max_distance: u32) -> Self {
        self.related_to(
            Direction::RightOf,
            Needle::Image(anchor.clone()),
            max_distance,
        )
    }

    pub fn left_of(self, anchor: &Pattern, max_distance: u32) -> Self {
        self.related_to(
            Direction::LeftOf,
            Needle::Image(anchor.clone()),
            max_distance,
        )
    }

    pub fn above(self, anchor: &Pattern, max_distance: u32) -> Self {
        self.related_to(
            Direction::Above,
            Needle::Image(anchor.clone()),
            max_distance,
        )
    }

    pub fn below(self, anchor: &Pattern, max_distance: u32) -> Self {
        self.related_to(
            Direction::Below,
            Needle::Image(anchor.clone()),
            max_distance,
        )
    }

    /// Restrict to things near the anchor, in any direction.
    pub fn near(self, anchor: &Pattern, max_distance: u32) -> Self {
        self.related_to(Direction::Near, Needle::Image(anchor.clone()), max_distance)
    }

    /// Give the anchor as a [`Needle`]. Use this to anchor on text.
    pub fn related_to(mut self, direction: Direction, anchor: Needle, max_distance: u32) -> Self {
        self.relation = Some(Relation {
            direction,
            anchor,
            max_distance,
        });
        self
    }

    pub fn needle(&self) -> &Needle {
        &self.needle
    }

    /// The image pattern. `None` for a text target.
    pub fn pattern(&self) -> Option<&Pattern> {
        self.needle.as_image()
    }

    pub fn region(&self) -> Region {
        self.region
    }

    /// A descriptive name, used in error messages.
    pub fn describe(&self) -> String {
        let mut s = self.needle.name();
        for alternative in &self.fallbacks {
            s.push_str(&format!(" → {}", alternative.name()));
        }
        if let Some(r) = &self.relation {
            s.push_str(&format!(
                " ({:?} '{}' within {}px)",
                r.direction,
                r.anchor.name(),
                r.max_distance
            ));
        }
        match self.selection {
            Selection::Best => {}
            Selection::Nth(n) => s.push_str(&format!(" [{n}]")),
            Selection::First => s.push_str(" [first]"),
            Selection::Last => s.push_str(" [last]"),
        }
        s
    }
}

/// Sort into reading order (top to bottom, left to right).
///
/// Sorting naively by y and then by x lets a tiny y difference split items that
/// belong to the same row. Items are grouped into rows with a tolerance of half
/// the pattern height first, then sorted by ascending x within each row.
pub(crate) fn sort_reading_order(matches: &mut [Match], row_tolerance: u32) {
    matches.sort_by_key(|m| (m.rect.y, m.rect.x));

    let tolerance = row_tolerance.max(1) as i32;
    let mut start = 0usize;
    while start < matches.len() {
        let row_top = matches[start].rect.y;
        let mut end = start + 1;
        while end < matches.len() && (matches[end].rect.y - row_top).abs() <= tolerance {
            end += 1;
        }
        matches[start..end].sort_by_key(|m| m.rect.x);
        start = end;
    }
}

/// Apply the ordering and the selection to narrow down to one.
///
/// `matches` must already be sorted by the caller (reading order, or distance
/// from the anchor). The tie handling in [`Selection::Best`] depends on that
/// order.
pub(crate) fn select_one(matches: Vec<Match>, selection: Selection) -> Option<Match> {
    match selection {
        // On a tie, take **the first in the order given**.
        //
        // `max_by` returns the last element on a tie, which makes the choice
        // effectively random. Image ZMD scores are continuous so ties are rare,
        // but **an exact OCR match is always 1.0000**, which mass-produces them.
        // In practice this grabbed the wrong one of two identical labels on screen.
        //
        // The caller sorts into reading order, so this becomes "on a tie, top
        // first and left first".
        Selection::Best => matches
            .into_iter()
            .fold(None, |best: Option<Match>, m| match &best {
                Some(b) if b.score >= m.score => best,
                _ => Some(m),
            }),
        Selection::First => matches.into_iter().next(),
        Selection::Last => matches.into_iter().next_back(),
        Selection::Nth(n) => matches.into_iter().nth(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(x: i32, y: i32, score: f32) -> Match {
        Match {
            rect: Rect::new(x, y, 20, 10),
            display: 0,
            score,
            target_offset: (0, 0),
        }
    }

    #[test]
    fn reading_order_groups_rows_before_sorting_by_x() {
        // Three that look like one row (y differs by 1px), plus one on the row below.
        let mut v = vec![
            m(300, 101, 0.9),
            m(100, 100, 0.8),
            m(200, 102, 0.7),
            m(150, 200, 0.95),
        ];
        sort_reading_order(&mut v, 5);
        let xs: Vec<i32> = v.iter().map(|m| m.rect.x).collect();
        assert_eq!(xs, vec![100, 200, 300, 150]);
    }

    #[test]
    fn reading_order_keeps_distinct_rows_separate() {
        let mut v = vec![m(10, 100, 0.9), m(5, 300, 0.9), m(20, 100, 0.9)];
        sort_reading_order(&mut v, 5);
        let ys: Vec<i32> = v.iter().map(|m| m.rect.y).collect();
        assert_eq!(ys, vec![100, 100, 300]);
    }

    #[test]
    fn selection_picks_expected_element() {
        let v = vec![m(0, 0, 0.5), m(10, 0, 0.9), m(20, 0, 0.7)];
        assert_eq!(select_one(v.clone(), Selection::First).unwrap().rect.x, 0);
        assert_eq!(select_one(v.clone(), Selection::Last).unwrap().rect.x, 20);
        assert_eq!(select_one(v.clone(), Selection::Nth(1)).unwrap().rect.x, 10);
        // Best chooses by score, not by sort order.
        assert_eq!(select_one(v.clone(), Selection::Best).unwrap().rect.x, 10);
        assert!(select_one(v, Selection::Nth(9)).is_none());
    }

    /// On a tie, take the first in the order given.
    ///
    /// An exact OCR match is always 1.0000, so ties are mass-produced. If this
    /// were undefined, the same text appearing twice on screen would be enough
    /// to click the wrong one — which happened in practice.
    #[test]
    fn best_breaks_ties_by_given_order() {
        // Assumes reading order. All three tie.
        let v = vec![m(100, 10, 1.0), m(200, 10, 1.0), m(50, 500, 1.0)];
        let picked = select_one(v, Selection::Best).unwrap();
        assert_eq!(
            (picked.rect.x, picked.rect.y),
            (100, 10),
            "something other than the first was chosen on a tie"
        );
    }

    /// Without a tie, the best score wins regardless of order.
    #[test]
    fn best_still_prefers_higher_score_over_order() {
        let v = vec![m(100, 10, 0.8), m(200, 10, 0.95), m(50, 500, 0.9)];
        let picked = select_one(v, Selection::Best).unwrap();
        assert_eq!(picked.rect.x, 200);
    }

    /// Combined with the reading-order sort, tie resolution becomes deterministic.
    #[test]
    fn reading_order_then_best_is_deterministic() {
        // Deliberately arranged in reverse reading order.
        let mut v = vec![m(50, 500, 1.0), m(200, 10, 1.0), m(100, 10, 1.0)];
        sort_reading_order(&mut v, 5);
        let picked = select_one(v, Selection::Best).unwrap();
        assert_eq!((picked.rect.x, picked.rect.y), (100, 10));
    }

    #[test]
    fn right_of_requires_vertical_overlap() {
        let anchor = Rect::new(100, 100, 50, 20); // 100..150 x 100..120
        // To the right, overlapping vertically.
        assert!(Direction::RightOf.accepts(anchor, Rect::new(160, 105, 40, 20), 100));
        // To the right but not overlapping vertically (far below).
        assert!(!Direction::RightOf.accepts(anchor, Rect::new(160, 400, 40, 20), 100));
        // Too far away.
        assert!(!Direction::RightOf.accepts(anchor, Rect::new(400, 105, 40, 20), 100));
        // To the left.
        assert!(!Direction::RightOf.accepts(anchor, Rect::new(10, 105, 40, 20), 100));
    }

    #[test]
    fn below_requires_horizontal_overlap() {
        let anchor = Rect::new(100, 100, 50, 20);
        assert!(Direction::Below.accepts(anchor, Rect::new(105, 130, 40, 20), 100));
        assert!(!Direction::Below.accepts(anchor, Rect::new(900, 130, 40, 20), 100));
        assert!(!Direction::Below.accepts(anchor, Rect::new(105, 50, 40, 20), 100));
    }

    #[test]
    fn near_ignores_direction() {
        let anchor = Rect::new(100, 100, 20, 20);
        // Any direction passes as long as it is within the distance.
        assert!(Direction::Near.accepts(anchor, Rect::new(100, 60, 20, 20), 60));
        assert!(Direction::Near.accepts(anchor, Rect::new(140, 100, 20, 20), 60));
        assert!(!Direction::Near.accepts(anchor, Rect::new(400, 400, 20, 20), 60));
    }

    #[test]
    fn describe_includes_relation_and_selection() {
        let img = mekiki_matching::Image::new(vec![0.5f32; 16], 4, 4);
        let p = Pattern::from_image(img.clone(), "field.png", 0.7, 3, 12);
        let a = Pattern::from_image(img, "label.png", 0.7, 3, 12);
        let t = Target::new(Region::new(Rect::new(0, 0, 100, 100), 0), Needle::Image(p))
            .right_of(&a, 200)
            .nth(2);
        let d = t.describe();
        assert!(d.contains("field.png"), "{d}");
        assert!(d.contains("label.png"), "{d}");
        assert!(d.contains("[2]"), "{d}");
    }

    /// A text target fits the same framework.
    #[test]
    fn text_needle_describes_itself() {
        let t = Target::new(
            Region::new(Rect::new(0, 0, 100, 100), 0),
            Needle::Text(TextPattern::new("保存", 0.8)),
        );
        assert!(t.describe().contains("ocr:保存"), "{}", t.describe());
        assert_eq!(t.needle().similarity(), 0.8);
        assert!(t.pattern().is_none(), "a text target has no image");
    }

    /// Text can be used as an anchor, so "the input field to the right of
    /// 'Name:'" is expressible.
    #[test]
    fn text_can_be_used_as_an_anchor() {
        let img = mekiki_matching::Image::new(vec![0.5f32; 16], 4, 4);
        let field = Pattern::from_image(img, "field.png", 0.7, 3, 12);
        let t = Target::new(
            Region::new(Rect::new(0, 0, 500, 500), 0),
            Needle::Image(field),
        )
        .related_to(
            Direction::RightOf,
            Needle::Text(TextPattern::new("名前", 0.8)),
            200,
        );
        let d = t.describe();
        assert!(d.contains("field.png"), "{d}");
        assert!(d.contains("ocr:名前"), "{d}");
    }

    /// similar() is written the same way and works for both images and text.
    #[test]
    fn similar_applies_to_both_needle_kinds() {
        let img = mekiki_matching::Image::new(vec![0.5f32; 16], 4, 4);
        let region = Region::new(Rect::new(0, 0, 100, 100), 0);

        let image_t = Target::new(
            region,
            Needle::Image(Pattern::from_image(img, "a.png", 0.7, 3, 12)),
        )
        .similar(0.95);
        assert_eq!(image_t.needle().similarity(), 0.95);

        let text_t = Target::new(region, Needle::Text(TextPattern::new("保存", 0.7))).similar(0.95);
        assert_eq!(text_t.needle().similarity(), 0.95);
    }
}

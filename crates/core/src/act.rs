//! Actions on a [`Target`], and the automatic wait that precedes them.
//!
//! The implementation of the [Rhai API execution model](../../../docs/architecture/rhai-api.md#execution-model).

use std::time::{Duration, Instant};

use mekiki_capture::{Frame, Rect};
use mekiki_input::{Key, Modifiers, MouseButton};
use mekiki_matching::Image;

use crate::model::{Match, Region, TextPattern, UiPattern};
use crate::target::{self, MatchOrder, Needle, Selection, Target};
use crate::{Error, FindFailed, Mekiki, Result, search};

/// The conditions to be met before an action.
///
/// The equivalent of Playwright's actionability. Image matching can decide two
/// of them: that the thing is visible, and that it is not moving.
///
/// **It cannot decide whether the click will be swallowed by another window.**
/// That gap is covered once the UI Automation hybrid (Phase 4-4 of the plan) is
/// in place.
#[derive(Copy, Clone, Debug)]
pub struct Actionability {
    /// How many consecutive identical frames of the match area count as
    /// "stable". 0 disables the check.
    ///
    /// Stops the accident of grabbing a dialog mid fade-in or a panel while it
    /// is sliding in.
    pub stable_frames: u32,
    /// The pixel-difference threshold for "identical" (a greyscale value in
    /// 0.0..=1.0).
    pub stable_tolerance: f32,
    /// More than this fraction of pixels changing counts as "moving".
    ///
    /// It is not 0 because something like a blinking caret in a text field
    /// changes only a handful of pixels but never stops, so it would never be
    /// considered stable.
    pub stable_max_changed_ratio: f32,
    /// The capture interval used for the stability check.
    pub stable_interval: Duration,
}

impl Default for Actionability {
    fn default() -> Self {
        Self {
            stable_frames: 1,
            stable_tolerance: 0.02,
            stable_max_changed_ratio: 0.005,
            stable_interval: Duration::from_millis(40),
        }
    }
}

impl Actionability {
    /// A configuration that performs no stability check.
    pub fn disabled() -> Self {
        Self {
            stable_frames: 0,
            ..Default::default()
        }
    }
}

/// The mechanism that skips searching by detecting that the screen has not
/// changed while waiting.
///
/// Phase 4-2 of the development plan.
///
/// # Why it helps
///
/// `wait` polls three times a second by default. Each poll costs tens of
/// milliseconds to capture and search, or hundreds for `ocr:`. Yet while
/// waiting, the screen has usually not changed at all.
///
/// The capture is unavoidable (you cannot know whether anything changed without
/// capturing), but **if nothing changed the search would return the same result
/// as last time**, so it can be skipped entirely.
///
/// # The test
///
/// Pixel difference against the previous frame. It uses the same measure as the
/// stability check ([`Actionability`]), so the thresholds mean the same thing.
#[derive(Copy, Clone, Debug)]
pub struct ChangeDetection {
    /// Whether it is enabled. When disabled, every poll searches.
    pub enabled: bool,
    /// The pixel-difference threshold for "identical".
    pub tolerance: f32,
    /// More than this fraction of pixels changing counts as "changed".
    pub max_changed_ratio: f32,
    /// Search again unconditionally once every this many polls, even with no
    /// change.
    ///
    /// Insurance against waiting forever on a change the difference test misses
    /// (a single pixel changing colour, say).
    pub force_rescan_every: u32,
}

impl Default for ChangeDetection {
    fn default() -> Self {
        Self {
            enabled: true,
            tolerance: 0.02,
            max_changed_ratio: 0.0005,
            force_rescan_every: 10,
        }
    }
}

impl ChangeDetection {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// State for one run of the wait loop.
#[derive(Default)]
struct WaitState {
    previous: Option<Vec<f32>>,
    skipped_since_rescan: u32,
    /// How many searches were skipped. For diagnostics.
    skipped_total: u32,
}

/// Progress through a search, used to diagnose a failure.
#[derive(Default)]
pub(crate) struct Attempt {
    pub best_score: Option<f32>,
    /// Whether the anchor was not found.
    pub anchor_missing: Option<String>,
    /// Whether it was found but never settled.
    pub unstable: bool,
    /// How many were dropped by the geometric conditions.
    pub filtered_out: usize,
}

/// One line of text read off the screen by [`Mekiki::read_text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLine {
    pub text: String,
    /// Where the line sits, in screen coordinates.
    pub rect: Rect,
}

/// One accessibility element enumerated by [`Mekiki::list_ui`].
///
/// This is the material for writing a `ui:` locator: the `name` is what
/// `ui:name=` matches, the `control_type` string is what `ui:type=` matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiItem {
    /// The accessible name — often the visible label, with any accelerator
    /// mnemonic attached, such as `保存(S)`.
    pub name: String,
    /// The automation id, when the application set one. More stable than the
    /// name; matched by `ui:id=`.
    pub automation_id: String,
    /// The control type, spelled as `ui:type=` expects (`button`, `edit`, ...).
    pub control_type: &'static str,
    /// Where the element sits, in screen coordinates.
    pub rect: Rect,
    pub enabled: bool,
}

/// A value read from one explicitly named accessibility element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiValue {
    pub name: String,
    pub automation_id: String,
    pub control_type: &'static str,
    pub rect: Rect,
    /// Redacted by the UIA backend when the element is a password field.
    pub value: Option<String>,
    pub source: Option<&'static str>,
    pub is_password: bool,
}

/// The temporary object returned by [`Mekiki::on`]. It merely borrows the
/// engine and the target.
pub struct Act<'a> {
    pub(crate) engine: &'a mut Mekiki,
    pub(crate) target: &'a Target,
}

impl Act<'_> {
    // -----------------------------------------------------------------------
    // Resolution
    // -----------------------------------------------------------------------

    /// Wait for actionability, then return one match.
    ///
    /// The returned [`Match`] is **a snapshot of this moment**. Holding on to
    /// the coordinates and using them later will not track changes on screen.
    /// Perform actions through [`Act`].
    pub fn resolve(self) -> Result<Match> {
        let Act { engine, target } = self;
        engine.wait_actionable(target)
    }

    /// Wait for actionability, then return every match meeting the conditions.
    ///
    /// The order comes from [`Target::order`] (reading order by default).
    pub fn resolve_all(self) -> Result<Vec<Match>> {
        let Act { engine, target } = self;
        let timeout = target.timeout.unwrap_or(engine.settings.auto_wait_timeout);
        let started = Instant::now();

        loop {
            let (matches, attempt) = engine.scan_target_all(target)?;
            if !matches.is_empty() {
                return Ok(matches);
            }
            if started.elapsed() >= timeout {
                return Err(engine.find_failed(target, &attempt, started.elapsed()));
            }
            std::thread::sleep(engine.settings.wait_scan_interval);
        }
    }

    /// Look once without waiting. Not finding anything is not an error.
    pub fn exists(self) -> Result<bool> {
        let Act { engine, target } = self;
        let (matches, _) = engine.scan_target(target)?;
        Ok(!matches.is_empty())
    }

    /// Wait until it disappears. `true` once it does, `false` on timeout.
    pub fn wait_vanish(self, timeout: Option<Duration>) -> Result<bool> {
        let Act { engine, target } = self;
        let timeout = timeout
            .or(target.timeout)
            .unwrap_or(engine.settings.auto_wait_timeout);
        let started = Instant::now();

        loop {
            let (matches, _) = engine.scan_target(target)?;
            if matches.is_empty() {
                return Ok(true);
            }
            if started.elapsed() >= timeout {
                return Ok(false);
            }
            std::thread::sleep(engine.settings.wait_scan_interval);
        }
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    pub fn hover(self) -> Result<Match> {
        let Act { engine, target } = self;
        let m = engine.wait_actionable(target)?;
        engine.hover(m.target())?;
        Ok(m)
    }

    pub fn click(self) -> Result<Match> {
        self.click_button(MouseButton::Left)
    }

    pub fn right_click(self) -> Result<Match> {
        self.click_button(MouseButton::Right)
    }

    pub fn click_button(self, button: MouseButton) -> Result<Match> {
        let Act { engine, target } = self;
        let m = engine.wait_actionable(target)?;
        engine.click_button(m.target(), button)?;
        Ok(m)
    }

    pub fn double_click(self) -> Result<Match> {
        let Act { engine, target } = self;
        let m = engine.wait_actionable(target)?;
        engine.double_click(m.target())?;
        Ok(m)
    }

    /// Click, then type a string.
    pub fn type_text(self, text: &str) -> Result<Match> {
        let Act { engine, target } = self;
        let m = engine.wait_actionable(target)?;
        engine.click(m.target())?;
        engine.type_text(text)?;
        Ok(m)
    }

    /// Move the mouse to the match, then press a key.
    pub fn press(self, key: Key, modifiers: Modifiers) -> Result<Match> {
        let Act { engine, target } = self;
        let m = engine.wait_actionable(target)?;
        engine.hover(m.target())?;
        engine.key_press(key, modifiers)?;
        Ok(m)
    }

    /// Move the mouse to the match, then scroll.
    pub fn scroll(self, horizontal: i32, vertical: i32) -> Result<Match> {
        let Act { engine, target } = self;
        let m = engine.wait_actionable(target)?;
        engine.hover(m.target())?;
        engine.scroll(horizontal, vertical)?;
        Ok(m)
    }

    /// Drag to another target.
    ///
    /// Both ends are resolved before the drag starts. Grabbing first and then
    /// searching for the destination would mean searching with the button held
    /// down.
    pub fn drag_to(self, destination: &Target) -> Result<(Match, Match)> {
        let Act { engine, target } = self;
        let from = engine.wait_actionable(target)?;
        let to = engine.wait_actionable(destination)?;
        engine.drag_drop(from.target(), to.target())?;
        Ok((from, to))
    }

    /// Outline the match on screen. For debugging.
    pub fn highlight(self, duration: Duration) -> Result<Match> {
        let Act { engine, target } = self;
        let m = engine.wait_actionable(target)?;
        engine.highlight_rect(m.rect, duration)?;
        Ok(m)
    }
}

// ---------------------------------------------------------------------------
// The resolution pipeline (the Mekiki-side implementation)
// ---------------------------------------------------------------------------

impl Mekiki {
    /// Resolve to a single match, including the automatic wait.
    pub(crate) fn wait_actionable(&mut self, target: &Target) -> Result<Match> {
        let mut wait_state = WaitState::default();
        let result = self.wait_actionable_loop(target, &mut wait_state);
        // Recorded on every exit so callers can see what change detection did.
        self.last_wait_skipped = wait_state.skipped_total;
        result
    }

    fn wait_actionable_loop(
        &mut self,
        target: &Target,
        wait_state: &mut WaitState,
    ) -> Result<Match> {
        let timeout = target.timeout.unwrap_or(self.settings.auto_wait_timeout);
        let started = Instant::now();

        // Progress from the most recent iteration that **actually searched**.
        //
        // If the timeout falls on an iteration where change detection skipped
        // the search, that iteration holds no information at all. Without
        // keeping it here, a known reason such as "the anchor was not found" or
        // "everything was dropped by distance" gets swallowed and degrades to a
        // bare "no candidate".
        let mut last_attempt = Attempt::default();

        // Total time spent paused, subtracted from the timeout check.
        //
        // Without subtracting it, **the search times out purely because it was
        // paused**. A pause is meant to stop the clock for debugging, not to
        // consume the wait budget.
        let mut paused_total = Duration::ZERO;

        loop {
            // Stop and pause are checked here. Nothing is held down while
            // waiting. The check during movement lives in `move_to`; stopping
            // mid-drag releases the button there.
            if self.interrupt.is_stopping() {
                return Err(Error::Interrupted);
            }
            paused_total += self.interrupt.wait_while_paused();
            if self.interrupt.is_stopping() {
                return Err(Error::Interrupted);
            }

            let elapsed = || started.elapsed().saturating_sub(paused_total);

            let want_all = target.relation.is_some() || target.selection != Selection::Best;

            // A `ui:`-only target never looks at pixels, so it must not depend
            // on a capture succeeding. Change detection goes with it: comparing
            // frames needs frames.
            let (matches, mut attempt) = if target.is_ui_only() {
                self.scan_ui_only(target, want_all)?
            } else {
                // Capture once, and use it for both the change test and the
                // search.
                //
                // Capturing separately for each would double the capture cost
                // whenever something did change, which is the normal case. At
                // 30ms for 4K that is not negligible.
                let frame = self.capture.capture_rect(target.region.rect)?;

                // Skip the search if the screen has not changed since last time
                // (Phase 4-2).
                if self.frame_unchanged(&frame, wait_state) {
                    if elapsed() >= timeout {
                        return Err(self.find_failed(target, &last_attempt, elapsed()));
                    }
                    std::thread::sleep(self.settings.wait_scan_interval);
                    continue;
                }

                self.scan_frame(target, &frame, want_all)?
            };

            if let Some(m) = target::select_one(matches, target.selection) {
                // The stability check compares captured pixels, so it cannot
                // run for a target that was resolved without a capture. UIA
                // reports an element's bounds directly rather than an
                // appearance that might still be settling, which is the closest
                // equivalent available here.
                let stable = target.force
                    || target.is_ui_only()
                    || self.settings.actionability.stable_frames == 0
                    || self.region_is_stable(m.rect)?;

                if stable {
                    return Ok(m);
                }
                attempt.unstable = true;
            }

            last_attempt = attempt;

            if elapsed() >= timeout {
                if wait_state.skipped_total > 0 {
                    log::debug!(
                        "'{}': skipped {} searches because the screen did not change",
                        target.describe(),
                        wait_state.skipped_total
                    );
                }
                return Err(self.find_failed(target, &last_attempt, elapsed()));
            }
            std::thread::sleep(self.settings.wait_scan_interval);
        }
    }

    /// Decide whether the search can be skipped, i.e. whether the screen is
    /// unchanged since last time.
    ///
    /// **The capture cannot be skipped**: there is no way to know whether
    /// anything changed without capturing. What can be skipped is the search,
    /// and since that dominates, this pays off. `ocr:` in particular costs
    /// several to a dozen times the capture.
    fn frame_unchanged(&mut self, frame: &Frame, state: &mut WaitState) -> bool {
        let cfg = self.settings.change_detection;
        if !cfg.enabled {
            return false;
        }

        // Insurance, so a change the difference test misses does not mean
        // waiting forever.
        if state.skipped_since_rescan >= cfg.force_rescan_every {
            state.skipped_since_rescan = 0;
            state.previous = None;
            return false;
        }

        let current = frame.to_luma_f32();

        let unchanged = match &state.previous {
            Some(prev) => frames_match(prev, &current, cfg.tolerance, cfg.max_changed_ratio),
            // Always search on the first pass.
            None => false,
        };

        state.previous = Some(current);
        if unchanged {
            state.skipped_since_rescan += 1;
            state.skipped_total += 1;
        } else {
            state.skipped_since_rescan = 0;
        }
        unchanged
    }

    /// Capture once and return the result, already filtered and sorted.
    ///
    /// How many are needed follows from the selection. When only one is wanted,
    /// exhaustive candidate collection can be skipped, so the default
    /// `Selection::Best` takes the fast path.
    pub(crate) fn scan_target(&mut self, target: &Target) -> Result<(Vec<Match>, Attempt)> {
        let want_all = target.relation.is_some() || target.selection != Selection::Best;
        self.scan_target_inner(target, want_all)
    }

    /// Collect **every** match meeting the conditions.
    ///
    /// `resolve_all` and count assertions use this. [`Mekiki::scan_target`]
    /// infers the required count from the selection, so with the default
    /// `Selection::Best` it returns only one.
    pub(crate) fn scan_target_all(&mut self, target: &Target) -> Result<(Vec<Match>, Attempt)> {
        self.scan_target_inner(target, true)
    }

    fn scan_target_inner(
        &mut self,
        target: &Target,
        want_all: bool,
    ) -> Result<(Vec<Match>, Attempt)> {
        // No capture for a target that never reads pixels — see `scan_ui_only`.
        if target.is_ui_only() {
            return self.scan_ui_only(target, want_all);
        }
        let frame = self.capture.capture_rect(target.region.rect)?;
        self.scan_frame(target, &frame, want_all)
    }

    /// Search within an already-captured frame.
    ///
    /// The capture is hoisted out so the wait loop can reuse the same frame for
    /// the change test.
    fn scan_frame(
        &mut self,
        target: &Target,
        frame: &Frame,
        want_all: bool,
    ) -> Result<(Vec<Match>, Attempt)> {
        let mut attempt = Attempt::default();

        // Resolve the anchor first: without it there is no point filtering
        // candidates.
        let anchor_rect = match &target.relation {
            None => None,
            Some(relation) => match self.locate_best(frame, target.region, &relation.anchor)? {
                Some(rect) => Some(rect),
                None => {
                    attempt.anchor_missing = Some(relation.anchor.name());
                    return Ok((Vec::new(), attempt));
                }
            },
        };

        // If the primary finds nothing, try the alternatives in the order
        // written (Phase 4-4).
        //
        // The test is "move on if nothing came back at all". The threshold cut
        // already happened inside locate_all, so empty means nothing met the
        // conditions.
        let mut scored = Vec::new();
        let mut used = &target.needle;
        for (index, needle) in target.needles().enumerate() {
            scored = self.locate_all(frame, target.region, needle, want_all)?;
            used = needle;
            if !scored.is_empty() {
                if index > 0 {
                    log::debug!(
                        "'{}' was not found; found via '{}' instead",
                        target.needle.name(),
                        needle.name()
                    );
                }
                break;
            }
        }

        attempt.best_score = scored
            .iter()
            .map(|(_, s)| *s)
            .fold(None::<f32>, |acc, s| Some(acc.map_or(s, |a| a.max(s))));

        Ok(self.finish_matches(target, scored, used, anchor_rect, attempt))
    }

    /// Turn scored rectangles into ordered matches.
    ///
    /// Shared by the pixel path and the accessibility-only path, which find
    /// candidates in completely different ways but must present them
    /// identically: the same anchor filtering, the same ordering, the same
    /// notion of where a click lands.
    fn finish_matches(
        &self,
        target: &Target,
        scored: Vec<(Rect, f32)>,
        used: &Needle,
        anchor_rect: Option<Rect>,
        mut attempt: Attempt,
    ) -> (Vec<Match>, Attempt) {
        // The offset comes from **the needle that was actually used**. Images
        // and UI elements produce rectangles in different ways, so a correction
        // applied to one must not be reused for the other.
        let mut matches: Vec<Match> = scored
            .into_iter()
            .map(|(rect, score)| Match {
                rect,
                display: target.region.display,
                score,
                target_offset: used.offset(),
            })
            .collect();

        // Filter by the geometric conditions and sort by proximity to the anchor.
        if let (Some(relation), Some(anchor)) = (&target.relation, anchor_rect) {
            let before = matches.len();
            matches.retain(|m| {
                relation
                    .direction
                    .accepts(anchor, m.rect, relation.max_distance)
            });
            attempt.filtered_out = before - matches.len();
            matches.sort_by(|a, b| {
                target::center_distance(anchor, a.rect)
                    .partial_cmp(&target::center_distance(anchor, b.rect))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        } else {
            let order = target.order.unwrap_or(self.settings.match_order);
            match order {
                MatchOrder::ReadingOrder => {
                    // The row tolerance comes from the candidate height,
                    // because a text target has no known height in advance.
                    let row_tolerance = matches
                        .first()
                        .map(|m| m.rect.height / 2)
                        .unwrap_or(8)
                        .max(1);
                    target::sort_reading_order(&mut matches, row_tolerance);
                }
                MatchOrder::Score => {
                    matches.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
            }
        }

        (matches, attempt)
    }

    /// Search using the accessibility API alone, **without capturing the screen**.
    ///
    /// `ui:` never reads pixels — it only needs the search area — yet the normal
    /// path captures a frame first and hands it down. That coupling means a
    /// broken capture takes the accessibility path with it, which is precisely
    /// when it would have been the way out: a display that cannot be duplicated,
    /// protected content, a remote session.
    ///
    /// Only used when [`Target::is_ui_only`] holds, so no caller can end up here
    /// needing pixels it was not given.
    fn scan_ui_only(&mut self, target: &Target, want_all: bool) -> Result<(Vec<Match>, Attempt)> {
        let area = target.region;
        let mut attempt = Attempt::default();

        let anchor_rect = match &target.relation {
            None => None,
            Some(relation) => {
                let Needle::Ui(ui) = &relation.anchor else {
                    unreachable!("is_ui_only() guarantees a ui: anchor");
                };
                let best = self
                    .locate_ui(area, ui)?
                    .into_iter()
                    .filter(|(_, score)| *score >= relation.anchor.similarity())
                    .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(rect, _)| rect);
                match best {
                    Some(rect) => Some(rect),
                    None => {
                        attempt.anchor_missing = Some(relation.anchor.name());
                        return Ok((Vec::new(), attempt));
                    }
                }
            }
        };

        let mut scored = Vec::new();
        let mut used = &target.needle;
        for (index, needle) in target.needles().enumerate() {
            let Needle::Ui(ui) = needle else {
                unreachable!("is_ui_only() guarantees every needle is ui:");
            };
            scored = self.locate_ui(area, ui)?;
            used = needle;
            if !scored.is_empty() {
                if index > 0 {
                    log::debug!(
                        "'{}' was not found; found via '{}' instead",
                        target.needle.name(),
                        needle.name()
                    );
                }
                break;
            }
        }

        attempt.best_score = scored
            .iter()
            .map(|(_, s)| *s)
            .fold(None::<f32>, |acc, s| Some(acc.map_or(s, |a| a.max(s))));

        let _ = want_all;
        Ok(self.finish_matches(target, scored, used, anchor_rect, attempt))
    }

    /// Search a single frame for a needle and return rectangles in screen
    /// coordinates along with scores.
    ///
    /// Images and text are searched in completely different ways, but returning
    /// the same shape lets the layers above (filtering, sorting, selection) be
    /// shared.
    fn locate_all(
        &mut self,
        frame: &Frame,
        scope: Region,
        needle: &Needle,
        want_all: bool,
    ) -> Result<Vec<(Rect, f32)>> {
        match needle {
            Needle::Image(pattern) => {
                if frame.width < pattern.width() || frame.height < pattern.height() {
                    // The area is smaller than the pattern; nothing to search.
                    return Ok(Vec::new());
                }
                let haystack = Image::new(frame.to_luma_f32(), frame.width, frame.height);
                let levels = search::build_haystack_pyramid(&haystack, pattern.max_level());
                let hits = search::find(
                    &mut self.matcher,
                    &levels,
                    pattern,
                    &self.settings.search,
                    want_all,
                );
                Ok(hits
                    .into_iter()
                    .map(|h| {
                        (
                            Rect::new(
                                frame.origin.0 + h.x as i32,
                                frame.origin.1 + h.y as i32,
                                pattern.width(),
                                pattern.height(),
                            ),
                            h.score,
                        )
                    })
                    .collect())
            }
            Needle::Text(text) => self.locate_text(frame, text),
            Needle::Ui(ui) => {
                let mut area = Region::new(
                    Rect::new(frame.origin.0, frame.origin.1, frame.width, frame.height),
                    scope.display,
                );
                area.window = scope.window;
                self.locate_ui(area, ui)
            }
        }
    }

    /// Search for a UI element via the accessibility API (Phase 4-4).
    ///
    /// **Takes only the search area, never a frame.** Not depending on
    /// appearance is the value of this path — it survives DPI scaling and theme
    /// changes — and taking a captured frame it did not read would have tied it
    /// to a capture that can fail on its own.
    ///
    /// Name comparison uses the same [`mekiki_ocr::match_score`] as OCR. A UIA
    /// name is not necessarily the displayed label (in practice you see names
    /// like "AppName - 1 running window, pinned"), so requiring an exact match
    /// would rule out the natural way of writing it.
    fn locate_ui(&mut self, area: Region, ui: &UiPattern) -> Result<Vec<(Rect, f32)>> {
        if ui.is_unconstrained() {
            // With no conditions, every element on screen would match. Better
            // to tell the author than to silently return a flood.
            return Err(Error::Uia(mekiki_uia::UiaError::Os(
                "ui: has no conditions; specify one of name= / id= / type=".into(),
            )));
        }

        let finder = match self.uia.as_mut() {
            Some(f) => f,
            None => match mekiki_uia::open() {
                Ok(f) => {
                    log::debug!("accessibility: {}", f.backend_name());
                    self.uia.insert(f)
                }
                Err(e) => return Err(Error::Uia(e)),
            },
        };

        let mut query = mekiki_uia::Query::new().within(mekiki_uia::Bounds::new(
            area.rect.x,
            area.rect.y,
            area.rect.width,
            area.rect.height,
        ));
        if let Some(hwnd) = area.window {
            query = query.rooted_at(hwnd);
        }
        if let Some(name) = ui.name() {
            query = query.with_name(name);
        }
        if let Some(id) = ui.automation_id() {
            query = query.with_automation_id(id);
        }
        if let Some(t) = ui.control_type() {
            query = query.with_control_type(t);
        }

        let elements = finder.find(&query).map_err(Error::Uia)?;

        let mut found = Vec::new();
        for e in elements {
            if !e.enabled && !ui.includes_disabled() {
                continue;
            }

            // With no name given, meeting the conditions (id / control type) is
            // treated as a perfect match. UIA has already done the filtering.
            let score = match ui.name() {
                Some(name) => {
                    // Score against the accessible name and against its
                    // mnemonic-stripped form, taking the better.
                    //
                    // On Japanese Windows almost every control's name carries an
                    // accelerator, so the Save button is 保存(S), not 保存.
                    // match_score is substring-based, so 保存 in 保存(S) scores
                    // 2/4 = 0.5 and falls under the threshold — writing the
                    // visible label always missed. Comparing against the
                    // stripped name lets the natural spelling match while the
                    // full name still does.
                    let raw = mekiki_ocr::match_score(name, &e.name);
                    let stripped = strip_mnemonic(&e.name);
                    if stripped == e.name {
                        raw
                    } else {
                        raw.max(mekiki_ocr::match_score(name, &stripped))
                    }
                }
                None => 1.0,
            };
            if score < ui.similarity() {
                continue;
            }

            found.push((
                Rect::new(e.bounds.x, e.bounds.y, e.bounds.width, e.bounds.height),
                score,
            ));
        }

        Ok(found)
    }

    /// Return only the best one. Used to resolve an anchor.
    fn locate_best(
        &mut self,
        frame: &Frame,
        scope: Region,
        needle: &Needle,
    ) -> Result<Option<Rect>> {
        let found = self.locate_all(frame, scope, needle, false)?;
        Ok(found
            .into_iter()
            .filter(|(_, score)| *score >= needle.similarity())
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(rect, _)| rect))
    }

    /// Search for text with OCR.
    ///
    /// Candidates are **runs of words within a line**, not whole lines. A whole
    /// line gives a wide rectangle whose centre is nowhere near the thing to
    /// click. Trying runs also picks up queries that span several words, such as
    /// "Save As".
    fn locate_text(&mut self, frame: &Frame, text: &TextPattern) -> Result<Vec<(Rect, f32)>> {
        let recognition = match self.recognize(frame)? {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };

        let query_len = mekiki_ocr::normalize(text.query()).chars().count().max(1);
        let mut found: Vec<(Rect, f32)> = Vec::new();

        for line in &recognition.lines {
            if text.is_whole_line() {
                if let Some((x, y, w, h)) = line.bounds() {
                    found.push((
                        Rect::new(frame.origin.0 + x, frame.origin.1 + y, w, h),
                        mekiki_ocr::match_score(text.query(), &line.text),
                    ));
                }
                continue;
            }

            // Try runs of words, stopping once the run is comfortably longer
            // than the query.
            for start in 0..line.words.len() {
                let mut combined = String::new();
                let mut left = i32::MAX;
                let mut top = i32::MAX;
                let mut right = i32::MIN;
                let mut bottom = i32::MIN;

                for word in &line.words[start..] {
                    combined.push_str(&word.text);
                    left = left.min(word.x);
                    top = top.min(word.y);
                    right = right.max(word.x + word.width as i32);
                    bottom = bottom.max(word.y + word.height as i32);

                    let score = mekiki_ocr::match_score(text.query(), &combined);
                    if right > left && bottom > top {
                        found.push((
                            Rect::new(
                                frame.origin.0 + left,
                                frame.origin.1 + top,
                                (right - left) as u32,
                                (bottom - top) as u32,
                            ),
                            score,
                        ));
                    }

                    // Past twice the query length, extending further can only
                    // lower the score.
                    if mekiki_ocr::normalize(&combined).chars().count() > query_len * 2 {
                        break;
                    }
                }
            }
        }

        // Keep only those at or above the threshold, taking the better of any
        // overlapping pair.
        found.retain(|(_, score)| *score >= text.similarity());
        found.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut kept: Vec<(Rect, f32)> = Vec::new();
        for (rect, score) in found {
            let overlaps = kept.iter().any(|(k, _)| k.intersect(&rect).is_some());
            if !overlaps {
                kept.push((rect, score));
            }
        }
        Ok(kept)
    }

    /// Read every line of text in a region, in screen coordinates.
    ///
    /// This is OCR used as **an eye rather than a search**: it dumps what is
    /// there instead of looking for one thing. [`Needle::Text`] answers "where
    /// is this string"; this answers "what does this area say".
    ///
    /// Lines, not words, because a line is the unit a reader thinks in. To
    /// click something, hand the text back as an `ocr:` target rather than
    /// aiming at a line rectangle — a long line's centre is nowhere near the
    /// word you meant, and the `ocr:` path matches runs of words for exactly
    /// that reason.
    ///
    /// An area below the OCR minimum yields an empty list, not an error, so
    /// scanning a small region is not a failure.
    pub fn read_text(&mut self, region: Region) -> Result<Vec<TextLine>> {
        let frame = self.capture.capture_rect(region.rect)?;
        let Some(recognition) = self.recognize(&frame)? else {
            return Ok(Vec::new());
        };

        Ok(recognition
            .lines
            .iter()
            .filter_map(|line| {
                let (x, y, w, h) = line.bounds()?;
                Some(TextLine {
                    text: line.text.clone(),
                    rect: Rect::new(frame.origin.0 + x, frame.origin.1 + y, w, h),
                })
            })
            .collect())
    }

    /// List the accessibility elements in a region.
    ///
    /// The counterpart of [`Self::read_text`] for the accessibility tree: a way
    /// to **see what is there** rather than confirm a guess. `find` with a `ui:`
    /// locator answers "is this here"; this answers "what names can I write".
    ///
    /// **It needs no screen capture**, so it works exactly where the pixel-based
    /// eyes do not — which is why it matters: without it, a `ui:` locator on a
    /// machine with broken capture means guessing the exact accessible name with
    /// nothing to guess from.
    ///
    /// `control_type` optionally filters to one kind (`"button"`, `"edit"`, ...).
    /// An unusable filter name is an error rather than an empty list, so a typo
    /// does not look like "nothing here".
    pub fn list_ui(&mut self, region: Region, control_type: Option<&str>) -> Result<Vec<UiItem>> {
        let type_filter = match control_type.map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(name) => Some(mekiki_uia::ControlType::parse(name).ok_or_else(|| {
                Error::Uia(mekiki_uia::UiaError::Os(format!(
                    "'{name}' is not a control type. One of: {}",
                    mekiki_uia::ControlType::NAMES
                )))
            })?),
        };

        let finder = match self.uia.as_mut() {
            Some(f) => f,
            None => match mekiki_uia::open() {
                Ok(f) => self.uia.insert(f),
                Err(e) => return Err(Error::Uia(e)),
            },
        };

        let mut query = mekiki_uia::Query::new().within(mekiki_uia::Bounds::new(
            region.rect.x,
            region.rect.y,
            region.rect.width,
            region.rect.height,
        ));
        if let Some(t) = type_filter {
            query = query.with_control_type(t);
        }
        if let Some(hwnd) = region.window {
            query = query.rooted_at(hwnd);
        }

        let elements = finder.find(&query).map_err(Error::Uia)?;
        Ok(elements
            .into_iter()
            .map(|e| UiItem {
                name: e.name,
                automation_id: e.automation_id,
                control_type: e.control_type.as_str(),
                rect: Rect::new(e.bounds.x, e.bounds.y, e.bounds.width, e.bounds.height),
                enabled: e.enabled,
            })
            .collect())
    }

    /// Read the value of exactly one explicitly named UIA element.
    ///
    /// Password fields are reported as password fields but their contents are
    /// never requested or returned. Ambiguity is an error: silently choosing
    /// one of several edit boxes is unsafe for automation and for disclosure.
    pub fn read_ui_value(&mut self, region: Region, ui: &UiPattern) -> Result<UiValue> {
        if ui.is_unconstrained() {
            return Err(Error::Uia(mekiki_uia::UiaError::Os(
                "read_value needs a constrained ui: locator".into(),
            )));
        }
        let finder = match self.uia.as_mut() {
            Some(f) => f,
            None => match mekiki_uia::open() {
                Ok(f) => self.uia.insert(f),
                Err(e) => return Err(Error::Uia(e)),
            },
        };
        let mut query = mekiki_uia::Query::new().within(mekiki_uia::Bounds::new(
            region.rect.x,
            region.rect.y,
            region.rect.width,
            region.rect.height,
        ));
        if let Some(hwnd) = region.window {
            query = query.rooted_at(hwnd);
        }
        if let Some(name) = ui.name() {
            query = query.with_name(name);
        }
        if let Some(id) = ui.automation_id() {
            query = query.with_automation_id(id);
        }
        if let Some(kind) = ui.control_type() {
            query = query.with_control_type(kind);
        }

        let mut elements: Vec<_> = finder
            .find(&query)
            .map_err(Error::Uia)?
            .into_iter()
            .filter(|e| e.enabled || ui.includes_disabled())
            .map(|e| {
                let score = match ui.name() {
                    None => 1.0,
                    Some(name) => {
                        let raw = mekiki_ocr::match_score(name, &e.name);
                        let stripped = strip_mnemonic(&e.name);
                        raw.max(mekiki_ocr::match_score(name, &stripped))
                    }
                };
                (e, score)
            })
            .filter(|(_, score)| *score >= ui.similarity())
            .collect();
        // When an exact name exists, lower-scoring fuzzy substrings are not
        // ambiguity. Keep all tied best candidates so genuinely indistinguish-
        // able controls still fail loudly.
        let best = elements
            .iter()
            .map(|(_, score)| *score)
            .fold(0.0f32, f32::max);
        elements.retain(|(_, score)| (best - *score).abs() < f32::EPSILON);
        if elements.len() != 1 {
            return Err(Error::Uia(mekiki_uia::UiaError::Os(format!(
                "read_value expected exactly one element for {}, found {}",
                ui.describe(),
                elements.len()
            ))));
        }
        let (e, _) = elements.remove(0);
        let read = finder.read_value(&e).map_err(Error::Uia)?;
        Ok(UiValue {
            name: e.name,
            automation_id: e.automation_id,
            control_type: e.control_type.as_str(),
            rect: Rect::new(e.bounds.x, e.bounds.y, e.bounds.width, e.bounds.height),
            value: read.value,
            source: read.source,
            is_password: read.is_password,
        })
    }

    /// Run OCR over a frame. The engine is created lazily and reused.
    fn recognize(&mut self, frame: &Frame) -> Result<Option<mekiki_ocr::Recognition>> {
        if self.ocr.is_none() {
            match mekiki_ocr::open() {
                Ok(engine) => {
                    log::info!(
                        "OCR: {} / language {}",
                        engine.backend_name(),
                        engine.language()
                    );
                    self.ocr = Some(engine);
                }
                Err(e) => return Err(Error::Ocr(e)),
            }
        }
        let engine = self.ocr.as_mut().expect("just created above");

        match engine.recognize_bgra(&frame.bgra, frame.width, frame.height) {
            Ok(r) => Ok(Some(r)),
            // An area below the OCR minimum (40x40) is treated as "no text".
            // Erroring here would mean a small search area alone breaks the run.
            Err(mekiki_ocr::OcrError::ImageSize { width, height }) => {
                log::debug!("too small to hand to OCR, treating as no text: {width}x{height}");
                Ok(None)
            }
            Err(e) => Err(Error::Ocr(e)),
        }
    }

    /// Check that the match area is not moving.
    ///
    /// Consecutive captures are compared, and `stable_frames` identical ones in
    /// a row means stable. A single change returns false immediately; waiting is
    /// the caller's loop's job.
    fn region_is_stable(&mut self, rect: Rect) -> Result<bool> {
        let cfg = self.settings.actionability;
        let mut previous = self.capture.capture_rect(rect)?.to_luma_f32();

        for _ in 0..cfg.stable_frames {
            std::thread::sleep(cfg.stable_interval);
            let current = self.capture.capture_rect(rect)?.to_luma_f32();
            if !frames_match(
                &previous,
                &current,
                cfg.stable_tolerance,
                cfg.stable_max_changed_ratio,
            ) {
                return Ok(false);
            }
            previous = current;
        }
        Ok(true)
    }

    pub(crate) fn find_failed(
        &mut self,
        target: &Target,
        attempt: &Attempt,
        waited: Duration,
    ) -> Error {
        let artifacts = self.write_failure_artifacts(target);

        Error::NotFound(Box::new(FindFailed {
            pattern: target.describe(),
            region: target.region.rect,
            best_score: attempt.best_score,
            required: target.needle.similarity(),
            waited,
            anchor_missing: attempt.anchor_missing.clone(),
            unstable: attempt.unstable,
            filtered_out: attempt.filtered_out,
            artifacts,
        }))
    }
}

/// Remove a Windows accelerator mnemonic like the `(S)` in 保存(S).
///
/// Windows shows a control's access key in its accessible name as a
/// parenthesised single character — `(S)`, `(Y)`, `(N)` — usually at the end.
/// A script writer types the label they see, which does not include it.
///
/// Deliberately narrow: only a single alphanumeric inside parentheses, and only
/// one such group, removed with the space that often precedes it. Anything
/// wider would eat real text like "Item (draft)".
fn strip_mnemonic(name: &str) -> String {
    let Some(open) = name.rfind(['(', '（']) else {
        return name.to_string();
    };
    let close = if name[open..].starts_with('（') {
        '）'
    } else {
        ')'
    };
    let Some(rel_close) = name[open..].find(close) else {
        return name.to_string();
    };
    let close_at = open + rel_close + close.len_utf8();

    let inner: Vec<char> = name[open..close_at]
        .chars()
        .skip(1) // opening bracket
        .take_while(|c| *c != close)
        .collect();
    if inner.len() != 1 || !inner[0].is_alphanumeric() {
        return name.to_string();
    }

    // Drop a single space just before the bracket too, so 保存 (S) also folds.
    let mut before = &name[..open];
    if before.ends_with([' ', '\u{3000}']) {
        before = &before[..before.len() - before.chars().last().unwrap().len_utf8()];
    }
    format!("{before}{}", &name[close_at..])
}

/// Whether two frames count as "the same".
///
/// This looks at **the fraction of pixels that changed**, not the maximum
/// difference. With maximum difference, something like a blinking caret in a
/// text field — where a handful of pixels change a lot — would never be
/// considered stable.
pub(crate) fn frames_match(a: &[f32], b: &[f32], tolerance: f32, max_changed_ratio: f32) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let changed = a
        .iter()
        .zip(b)
        .filter(|(x, y)| (*x - *y).abs() > tolerance)
        .count();
    (changed as f32 / a.len() as f32) <= max_changed_ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows accelerator mnemonic is folded so the visible label matches.
    #[test]
    fn strip_mnemonic_removes_a_single_accelerator() {
        // The case from the field: Japanese Save button.
        assert_eq!(strip_mnemonic("保存(S)"), "保存");
        assert_eq!(strip_mnemonic("はい(Y)"), "はい");
        // A space before the bracket is folded too.
        assert_eq!(strip_mnemonic("Save (S)"), "Save");
        // Full-width parentheses, as Japanese UIs sometimes use.
        assert_eq!(strip_mnemonic("保存（S）"), "保存");
        // And it must actually let the natural spelling score as a match.
        assert!(mekiki_ocr::match_score("保存", &strip_mnemonic("保存(S)")) > 0.99);
    }

    /// It must not eat real parenthetical text.
    #[test]
    fn strip_mnemonic_leaves_ordinary_parentheses_alone() {
        assert_eq!(strip_mnemonic("Item (draft)"), "Item (draft)");
        assert_eq!(strip_mnemonic("Zoom (100%)"), "Zoom (100%)");
        assert_eq!(strip_mnemonic("plain"), "plain");
        // A single character that is not alphanumeric is not a mnemonic.
        assert_eq!(strip_mnemonic("A (!)"), "A (!)");
    }

    #[test]
    fn identical_frames_match() {
        let a = vec![0.5f32; 100];
        assert!(frames_match(&a, &a, 0.02, 0.005));
    }

    #[test]
    fn wholesale_change_is_detected() {
        let a = vec![0.2f32; 100];
        let b = vec![0.8f32; 100];
        assert!(!frames_match(&a, &b, 0.02, 0.005));
    }

    #[test]
    fn a_single_blinking_pixel_is_tolerated() {
        // One pixel in 1000 changing a lot: the blinking-caret case.
        let a = vec![0.5f32; 1000];
        let mut b = a.clone();
        b[0] = 1.0;
        assert!(
            frames_match(&a, &b, 0.02, 0.005),
            "a blinking caret would never be considered stable"
        );
    }

    #[test]
    fn many_small_changes_are_detected() {
        // 5% of pixels changing: the spinner or progress bar case.
        let a = vec![0.5f32; 1000];
        let mut b = a.clone();
        for v in b.iter_mut().take(50) {
            *v = 0.9;
        }
        assert!(!frames_match(&a, &b, 0.02, 0.005));
    }

    #[test]
    fn subthreshold_noise_is_ignored() {
        let a = vec![0.5f32; 1000];
        let b: Vec<f32> = a.iter().map(|v| v + 0.01).collect();
        assert!(frames_match(&a, &b, 0.02, 0.005));
    }

    #[test]
    fn length_mismatch_is_not_stable() {
        assert!(!frames_match(&[0.5; 10], &[0.5; 9], 0.02, 0.005));
        assert!(!frames_match(&[], &[], 0.02, 0.005));
    }

    #[test]
    fn disabled_actionability_has_zero_frames() {
        assert_eq!(Actionability::disabled().stable_frames, 0);
    }
}
